# M6F — Multi-vendor gNMI/YANG interoperability acceptance

L4 lab harness for real-NOS gNMI/YANG interoperability. Not part of
L0–L3; never run by CI. Requires operator-invoked execution on a host
with containerlab and Docker.

## Layout

- `manifests/vendors.yaml` — operator-maintained vendor/image/endpoint
  manifest. Contains **no secrets**: credentials are env-var *names*.
- `configs/` — per-vendor startup configs (lab-only, read-only intent).
- `tools/m6f_gnmi.py` — read-only gNMI client (Capabilities, YANG Library
  Get RFC 8525 + legacy, one-shot Subscribe). Generates stubs from
  `proto/gnmi.proto` at run time; implements **no Set RPC**.
- `tools/m6f_manifest.py` — manifest resolver.
- `tools/m6f_image.py` — deterministic docker-archive → image
  resolution. Reads the archive's own docker-save `manifest.json`
  (including a single nested docker archive, e.g. the XRd wrapper
  .tgz), inspects ONLY the references the archive declares, skips
  `docker load` when a compatible image is already present, and never
  selects a globally-newest image. Multiple distinct images in one
  archive require an explicit `image_select`/manifest `image` choice.
- `../../scripts/lab/m6f-gnmi-interop.sh` — orchestrator.

## Usage

```sh
# preflight only (disk/mem/image/credential checks; no deploy)
scripts/lab/m6f-gnmi-interop.sh --vendor srlinux --action preflight

# full slice: preflight -> deploy -> probe -> cleanup
scripts/lab/m6f-gnmi-interop.sh --vendor srlinux --action all

# keep the lab running for interactive debugging
scripts/lab/m6f-gnmi-interop.sh --vendor srlinux --action all --keep

# destroy only the m6f-<vendor> topology
scripts/lab/m6f-gnmi-interop.sh --vendor srlinux --action cleanup
```

Python: the script uses `$BONSAI_PYTHON`, else `<repo>/.venv/bin/python`,
else `python3`. The venv needs `grpcio`, `grpcio-tools`, `protobuf`,
`pyyaml`.

## Credentials

Set the env vars named in the manifest before running, e.g.:

```sh
export M6F_SRL_USER=admin M6F_SRL_PASS=admin   # lab-local values only
```

Values are passed to the gNMI client via env-var indirection and are
never written to evidence files.

## Images

- `srlinux`: public image pinned by digest (same family as
  `lab/acceptance/bn006-srl`). Pass `--allow-pull` to permit pulling this
  public ghcr image; otherwise it must already be present.
- `xrd`, `junos`, `ceos`: owner-supplied. Populate `image` or
  `image_archive` in the manifest once the owner images under
  `~/Desktop/clab-images/` are imported. The harness resolves the exact
  archive-declared reference via `tools/m6f_image.py` and `docker load`s
  only when no compatible image is already present — **no source changes
  needed**. Proprietary images are never pulled and owner tags are never
  modified. Archives carrying multiple distinct images require an
  explicit `image_select` manifest field.

## TLS semantics

`tools/m6f_gnmi.py` makes transport security explicit — there is no
silent downgrade:

- `--tls-mode verified` (default): certificate-verified TLS. `--ca`
  selects the trust root (system roots when omitted); `--server-name`
  sets SNI *and* the expected certificate identity; `--client-cert` /
  `--client-key` enable mutual TLS where required.
- `--tls-mode plaintext`: explicit lab-only cleartext channel.
  `--insecure` is a deprecated alias for this — it does **not** mean
  "TLS without certificate verification".
- `--tls-mode unverified`: rejected — unverified TLS is not supported.
- Missing `--ca`/cert files fail closed before any RPC.

The orchestrator maps manifest `gnmi.tls: clab_ca` to verified TLS with
the lab CA discovered under the run's own workdir and the manifest
`gnmi.tls_server_name`; `gnmi.tls: insecure` maps to explicit plaintext.
A missing lab CA is an error, never a plaintext fallback.

## Evidence

Each run writes `docs/merger/evidence/<UTC-date>-m6f-<vendor>/`:

- `capabilities.json` — gNMI Capabilities response (models, encodings,
  gNMI version).
- `yang-library-ietf.json` — RFC 8525 `/yang-library:yang-library` Get.
- `yang-library-legacy.json` — legacy `/modules-state` Get.
- `subscribe-once.json` — one-shot Subscribe on a conservative
  read-only path.
- `probe-result.json`, `result.json` — per-check
  PASS/FAIL/UNSUPPORTED/NOT_EXECUTED and overall status.
- `topology.used.yml`, `deploy.log`, `destroy.log`, `disk-before.txt`,
  `mem.txt`, `image-identity.txt`, `SHA256SUMS`.

An unavailable vendor/image is reported `NOT_EXECUTED`, never green.
Captured evidence is sanitized: no credentials, keys or tokens.

## Status semantics

| Status | Meaning |
|---|---|
| PASS | RPC executed and returned valid evidence |
| FAIL | RPC executed but errored / malformed |
| UNSUPPORTED | device responded but lacks the capability |
| NOT_EXECUTED | precondition missing (image, creds, reachability) |

Overall result is `PASS` only when every check PASSes; any
UNSUPPORTED/NOT_EXECUTED check yields overall `PARTIAL`, any FAIL
yields `FAIL`. Per-check outcome states are recorded under
`outcome_states` in `result.json`/`probe-result.json`.

See `MATRIX.md` for the per-vendor capability matrix.

## Production Core/collector acceptance (M6F-B)

`scripts/lab/m6f-bonsai-e2e.sh` runs the full production path against
the deployed lab device — not the Python probe:

```sh
M6F_ADMIN_USER=… M6F_ADMIN_PASS=… \
M6F_PLAN_FILE=/path/to/plan.json \
scripts/lab/m6f-bonsai-e2e.sh --action all
```

Actions: `preflight`, `tls`, `configs`, `deploy`, `yang`, `isolation`,
`drive`, `evidence`, `teardown`, `all`. The `drive` action runs
`tools/m6f_e2e.py`, which executes the authenticated Admin API sequence:
collector binding → device registration → assignment → YANG catalogue
compile → Core-issued remote discovery job → applicability snapshot →
schema-bound plan preview → explicit activation → telemetry observation
poll → revocation → withdrawal verification. Every stage records an
explicit PASS/FAIL; an incomplete stage cannot yield overall PASS.

`M6F_PLAN_FILE` is a JSON document with `datastore` and
`selected_paths` (path/origin/mode/sample_interval_ns/rationale). The
selected paths must be *applicable schema paths at the granularity the
device actually reports*: SR Linux emits notifications at container
granularity with module-prefixed element names
(`srl_nokia-interfaces:interface[name=mgmt0]/statistics`), so the
accepted M6F-B plan selects `/interface[name=mgmt0]/statistics`
(SAMPLE, 10 s) — leaf-level expectations under it are not covered by
container-granular updates. A working example is committed at
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r7/plan.json`.

Evidence lands in `docs/merger/evidence/<date>-m6fb-srlinux-rN/`:
per-stage API JSON (`api/NN-*.json`), `collector.log`, `core.log`,
`runtime-state.txt`, `stages.json`, `plan.json`. The collector log
shows the exact SubscribeRequest parameters (`paths=N plan_id=…
acquisition="explicit"`); no `Set` RPC exists anywhere in the
collector code path.

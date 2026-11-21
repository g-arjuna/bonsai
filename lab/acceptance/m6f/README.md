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
  `~/Desktop/clab-images/` are imported. The harness `docker load`s the
  archive and resolves the reference — **no source changes needed**.
  Proprietary images are never pulled and owner tags are never modified.

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

See `MATRIX.md` for the per-vendor capability matrix.

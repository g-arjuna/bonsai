# bonsai Helm chart

D4-T1 (DV1) — Kubernetes Helm chart for deploying bonsai in single, HA, or collector-fleet mode.

## Quick start

```bash
# Single-node (default, for lab/PoC)
helm install bonsai ./deploy/helm/bonsai -f deploy/helm/bonsai/values-single.yaml

# HA core (StatefulSet, 2 replicas)
helm install bonsai ./deploy/helm/bonsai -f deploy/helm/bonsai/values-ha.yaml

# Fleet (bonsai-core + 3 collector replicas)
helm install bonsai ./deploy/helm/bonsai -f deploy/helm/bonsai/values-fleet.yaml
```

## Modes

| Mode | Workload | Use case |
|---|---|---|
| `single` | Deployment (1 replica) | Lab, PoC, CI |
| `ha` | StatefulSet (N replicas) | Production-like, persistent volumes per pod |
| `fleet` | Deployment (core) + Deployment (collector) | Large topology with horizontal collector scaling |

## Templates

| File | Description |
|---|---|
| `_helpers.tpl` | Named template helpers (fullname, labels, image refs) |
| `configmap.yaml` | Mounts `bonsai.toml` from chart values |
| `secret.yaml` | `BONSAI_VAULT_PASSPHRASE` — skipped if `existingSecret` is set |
| `serviceaccount.yaml` | Optional ServiceAccount |
| `service.yaml` | ClusterIP Service (http/grpc/metrics ports) |
| `pvc.yaml` | PVCs for archive + graph (single/fleet mode only) |
| `deployment.yaml` | Deployment (single and fleet core) |
| `statefulset.yaml` | StatefulSet with volumeClaimTemplates (ha mode only) |

## Key values

```yaml
mode: single                   # single | ha | fleet
image.repository: bonsai
image.tag: latest
config.gnnInferenceMode: calibration   # calibration | production
persistence.enabled: true
persistence.archive.size: 20Gi
persistence.graph.size: 5Gi
sidecar.enabled: false         # set true to co-deploy the Python rules sidecar
secrets.existingSecret: ""     # name of an existing K8s Secret, or "" to auto-create
```

## Credentials

Never commit credentials. Either:
1. Pre-create a Kubernetes Secret and set `secrets.existingSecret: my-bonsai-secret`, or
2. Pass at install time: `--set secrets.credentialPassphrase=<passphrase>`

## Linting

```bash
helm lint deploy/helm/bonsai
helm template bonsai deploy/helm/bonsai -f deploy/helm/bonsai/values-single.yaml | less
helm template bonsai deploy/helm/bonsai -f deploy/helm/bonsai/values-ha.yaml | less
```

> **Note**: The IDE YAML linter reports errors on Helm template files because it
> parses them as raw YAML before Go template rendering. These errors are
> expected and harmless — `helm lint` and `helm template` are the correct tools.

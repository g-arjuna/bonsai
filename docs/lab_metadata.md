# Lab Metadata Convention

CV1 makes topology metadata part of the ingestion contract rather than an
informal naming habit. Every maintained lab topology should declare:

- `bonsai.role` on every node
- `bonsai.environment` on every node

For ContainerLab topologies we store both values under each node's `labels`
block so the metadata stays close to the node definition and remains safe for
existing deploy workflows.

## Required labels

```yaml
topology:
  nodes:
    srl-leaf1:
      kind: nokia_srlinux
      labels:
        bonsai.role: leaf
        bonsai.environment: data_center
```

## Allowed environment values

- `data_center`
- `service_provider`
- `campus_wired`

One topology should use a single shared environment value across all of its
nodes. Mixed-role labs are fine. Mixed-environment labs are not.

## Recommended role values

- `super-spine`
- `spine`
- `leaf`
- `pe`
- `p`
- `rr`
- `ce`
- `access`
- `distribution`
- `core`
- `edge`

For non-network endpoints such as traffic generators, use `host`.

## Why this exists

- The GNN loader needs a stable role vocabulary instead of hostname-only
  guesses.
- The future synthesizer needs explicit environment context for rule selection.
- Lab fixtures become machine-checkable, which helps keep CV1 metadata drift out
  of the repo.

The unit test at `python/tests/test_lab_metadata.py` enforces this convention
for the topologies currently used by Bonsai.

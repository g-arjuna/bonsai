# Chaos Plans

These plans drive `scripts/chaos_runner.py` against the Phase 4 `bonsai-p4` lab.
Run the chaos runner from WSL inside the repo-local `.venv/`, because `clab` and the
live ContainerLab topology are hosted there.

`baseline_mix.yaml`
Use this first. It mixes BGP session drops, interface shutdowns, and `netem` loss events with moderate pacing. Best for broad training data and end-to-end smoke runs.

`bgp_heavy.yaml`
Biases strongly toward BGP session loss/recovery to accumulate `bgp_session_down` detections and remediation outcomes faster for Model C training.

`gradual_only.yaml`
Forward-looking plan for T3-4. It documents the intended input shape for `gradual_degradation` events, but today `scripts/chaos_runner.py` does not implement that fault type yet, so the runner will warn and skip it until T3-4 lands.

Notes

- Host/peer/interface combinations are topology-specific. These plans are written for `lab/fast-iteration/bonsai-phase4.clab.yml`.
- `interface_shut` uses NOS interface names (`ethernet-1/x`, `GigabitEthernet0/0/0/x`).
- `netem_loss` uses ContainerLab link endpoint names (`e1-1`, `e1-2`, `e1-3`) because it operates through `clab tools netem`.
- If `clab` is not installed on the host that runs `scripts/chaos_runner.py`, the runner will warn and skip `netem_loss` entries instead of failing the whole plan. The intended production path for these plans is still WSL, not Windows-hosted Python.
- For a quick smoke run:
  `python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --duration-hours 0.03`

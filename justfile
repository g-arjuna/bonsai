# Bonsai + RustyBMP Unified Development & Verification Runner (B20)

set shell := ["bash", "-euo", "pipefail", "-c"]

# Non-mutating dependencies, ports, disk and image checks
doctor:
    rustc --version
    cargo --version
    node --version
    npm --version
    python3 --version
    df -h .
    DOCKER_HOST=unix:///var/run/docker.sock docker version --format '{{.Server.Version}}'
    DOCKER_HOST=unix:///var/run/docker.sock docker compose version
    containerlab version

# Fast protocol, vendor registry, ML/eval truthfulness, and routing contract checks
check-fast:
    cargo test -p rbmp-core -p rbmp-rib -p rbmp-store --offline
    PYTHONPATH=.:python .venv/bin/pytest tests/test_b07_vendor_registry.py tests/test_b15_ssh_host_key_policy.py tests/test_a01_conformal_and_eval_metrics.py tests/test_a02_stgnn_entity_alignment.py -v

# Real parser golden, negative, and RFC contract corpus
test-protocol:
    cargo test -p rbmp-core --offline
    cargo test --package bonsai --test bmp_rfc_contract --offline
    python3 docs/merger/evidence/2026-10-11/inspect_fixtures.py crates/rbmp-core

# Journal, RIB, history equivalence, RBAC, investigation runtime, and M3 oracle/load/migration gates
test-replay:
    cargo test -p rbmp-rib -p rbmp-store --offline
    cargo test --package bonsai --test b09_peer_and_correlation --offline
    cargo test --package bonsai --test b11_b12_b13_routing_workflow --offline
    cargo test --package bonsai --test b16_b17_b18_b19_m3_release_gates --offline
    cargo test --package bonsai --test a03_investigation_terminal_states --offline

# Start minimal unified product deployment (Bonsai + bonsai-routing)
dev-up:
    DOCKER_HOST=unix:///var/run/docker.sock docker compose --profile standalone up -d

# Stop minimal unified product deployment
dev-down:
    DOCKER_HOST=unix:///var/run/docker.sock docker compose --profile standalone down

# Run FRR expected-state oracle and two-run reproducibility verification
lab-test target="frr":
    cargo test --package bonsai --test b16_b17_b18_b19_m3_release_gates b17_b18_frr_two_clean_runs_and_multivendor_capability_matrix --offline -- --exact --nocapture

# Full release verification gate across protocol, replay, UI build, Python policies, and Compose config
verify-release:
    just test-protocol
    just test-replay
    PYTHONPATH=.:python .venv/bin/pytest tests/test_b07_vendor_registry.py tests/test_b15_ssh_host_key_policy.py tests/test_a01_conformal_and_eval_metrics.py tests/test_a02_stgnn_entity_alignment.py -v
    cd ui && npm run build
    BONSAI_VAULT_PASSPHRASE=verify-passphrase DOCKER_HOST=unix:///var/run/docker.sock docker compose --profile standalone config -q


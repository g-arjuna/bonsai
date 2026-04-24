# Sprint 4 Verification Plan & Testing Results

## 1. Testing Results

**Environment Audit & Infrastructure**
- Verified Docker compose distributed stack (`bonsai-core`, `bonsai-collector-1`, `bonsai-collector-2`).
- Fixed healthcheck syntax in `docker-compose.yml` to use `["CMD", "healthcheck"]` (list form) instead of string/shell form.
- Addressed a permissions issue on mTLS keys (`chmod 644`) to ensure the container user can read certificates.
- Updated `generate_compose_tls.sh` to ensure future certificates are created with readable permissions.
- Fixed an incomplete TOML configuration for collectors (`graph_path` missing at root level).
- Identified and fixed a bug in `src/ingest.rs` where `run_collector_manager` lacked a retry loop on initial connection failure, causing the assignment stream to permanently fail on startup if the core was not immediately ready.

**Functional Verification**
- **Credentials Vault**: Successfully seeded Nokia SR Linux and Cisco XRd credentials via the REST API on the core.
- **mTLS Communication**: Confirmed `bonsai-collector-1` and `bonsai-collector-2` successfully authenticate and stream data to `bonsai-core` over mTLS. 
- **Python SDK Growth**: Added TLS configuration support to the Python `BonsaiClient`. Updated `demo_phase4.py` to read cert paths from environment variables.
- **Topology Discovery**: Verified the core API returns the full lab topology (`srl-leaf1`, `srl-leaf2`, `srl-spine1`, `xrd-pe1`) with all expected links and BGP peerings.
- **Fault Detection (End-to-End)**: 
  - Ran the `demo_phase4.py` Python rule engine with mTLS enabled.
  - Injected a fault by administratively disabling `ethernet-1/1` on `srl-leaf1`.
  - The graph successfully updated device health to `warn`.
  - The `interface_down` detection rule fired twice (once for each side of the link), and `DetectionEvent` nodes were correctly written to the graph with detailed features.

**Cleanup**
- Confirmed no stale or dangling Docker images exist (`<none>`).
- Retained `bonsai:latest` image for immediate use.
- Distributed stack gracefully shut down after testing.

## 2. Conclusion
Sprint 4 is complete. The containerised distributed stack is stable, secure via mTLS, and successfully detects faults injected in a multi-vendor lab environment. 

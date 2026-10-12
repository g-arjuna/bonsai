/// Integration test harness for rustybmp (RV4-9).
///
/// bmp_pdu: pure parse-layer tests using rbmp_core only (no server needed).
/// api_smoke / retention_sweep are historical, uncompiled harness sketches:
/// they reference a nonexistent rbmp-server library and outdated APIs.
/// Live API coverage runs through tests/api/ against the disposable CI backend.
///
/// Run with:
///   cargo test -p rbmp-core --test integration --locked -- --nocapture
pub mod bmp_pdu;
pub mod frr_bmp;
pub mod mcp_tools;

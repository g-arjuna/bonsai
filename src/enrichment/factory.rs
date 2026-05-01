//! Build a concrete `GraphEnricher` from a persisted `EnricherConfig`.

use anyhow::{bail, Result};

use super::{EnricherConfig, GraphEnricher, StubEnricher};
use super::netbox::NetBoxEnricher;
use super::servicenow::ServiceNowEnricher;

/// Instantiate the right enricher for the given config.
///
/// `enricher_type` values:
/// - `"netbox"`      → `NetBoxEnricher` (REST or MCP transport)
/// - `"servicenow"`  → `ServiceNowEnricher` (CMDB via Table API)
/// - `"stub"`        → `StubEnricher` (tests / CI)
pub fn build_enricher(config: &EnricherConfig) -> Result<Box<dyn GraphEnricher>> {
    match config.enricher_type.as_str() {
        "netbox" => Ok(Box::new(NetBoxEnricher::from_config(config.clone()))),
        "servicenow" => Ok(Box::new(ServiceNowEnricher::from_config(config.clone()))),
        "stub" => Ok(Box::new(StubEnricher { name: config.name.clone() })),
        other => bail!("unknown enricher_type '{other}'"),
    }
}

use std::sync::Arc;

use anyhow::{Result, bail};

use crate::config::{LayeredIngestionConfig, TargetConfig};
use crate::credentials::ResolvedCredential;
use crate::enrichment::multi_source::{
    GnmiGetConfigEnricher, MultiSourceCapture, MultiSourceEnricher,
};
use crate::enrichment::parser_chain_enricher::ParserChainCliEnricher;
use crate::enrichment::parser_chain_enricher::inferred_vendor;

pub struct MultiSourceEnricherRegistry {
    gnmi: Arc<dyn MultiSourceEnricher>,
    parser_chain: Arc<dyn MultiSourceEnricher>,
}

impl MultiSourceEnricherRegistry {
    pub fn from_layered_config(config: &LayeredIngestionConfig) -> Self {
        Self {
            gnmi: Arc::new(GnmiGetConfigEnricher {
                paths: config.default_gnmi_get_paths.clone(),
            }),
            parser_chain: Arc::new(ParserChainCliEnricher::new(config.parser_chain.clone())),
        }
    }

    pub async fn capture(
        &self,
        target: &TargetConfig,
        credentials: Option<&ResolvedCredential>,
    ) -> Result<MultiSourceCapture> {
        let mut failures = Vec::new();
        for enricher in self.capture_plan(target) {
            match enricher.capture(target, credentials).await {
                Ok(capture) => return Ok(capture),
                Err(error) => failures.push(format!("{}: {error:#}", enricher.name())),
            }
        }
        bail!(
            "all multi-source capture strategies failed for {} ({})",
            target.address,
            failures.join("; ")
        )
    }

    fn capture_plan(&self, target: &TargetConfig) -> Vec<Arc<dyn MultiSourceEnricher>> {
        if prefers_cli_first(target) {
            vec![Arc::clone(&self.parser_chain), Arc::clone(&self.gnmi)]
        } else {
            vec![Arc::clone(&self.gnmi), Arc::clone(&self.parser_chain)]
        }
    }
}

fn prefers_cli_first(target: &TargetConfig) -> bool {
    let vendor = inferred_vendor(target);
    matches!(
        vendor.as_str(),
        "cisco-iosxr" | "juniper-junos" | "arista-eos" | "frr"
    ) && target.ca_cert.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(vendor: &str, tls: bool) -> TargetConfig {
        TargetConfig {
            address: "172.100.102.21:57400".to_string(),
            enabled: true,
            tls_domain: tls.then(|| "device.local".to_string()),
            ca_cert: tls.then(|| "lab/ca.pem".to_string()),
            vendor: Some(vendor.to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: None,
            role: None,
            site: None,
            collector_id: None,
            selected_paths: Vec::new(),
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
        }
    }

    #[test]
    fn cli_first_prefers_non_tls_multi_vendor_targets() {
        assert!(prefers_cli_first(&target("cisco-iosxr", false)));
        assert!(prefers_cli_first(&target("juniper-junos", false)));
        assert!(!prefers_cli_first(&target("nokia-srlinux", true)));
        assert!(!prefers_cli_first(&target("nokia-srlinux", false)));
    }
}

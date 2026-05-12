use std::sync::Arc;

use anyhow::{Result, bail};

use crate::config::{LayeredIngestionConfig, TargetConfig};
use crate::credentials::ResolvedCredential;
use crate::discovery::GnmiReadinessReport;
use crate::enrichment::multi_source::{
    GnmiGetConfigEnricher, MultiSourceCapture, MultiSourceEnricher,
};
use crate::enrichment::parser_chain_enricher::ParserChainCliEnricher;

/// Ordered list of enrichers to try for a given target.
/// The registry is a `Vec` so future enrichers (BMP-derived state, REST APIs,
/// BGP-LS topology metadata) can be registered without touching this file.
pub struct MultiSourceEnricherRegistry {
    enrichers: Vec<Arc<dyn MultiSourceEnricher>>,
}

impl MultiSourceEnricherRegistry {
    pub fn from_layered_config(config: &LayeredIngestionConfig) -> Self {
        Self {
            enrichers: vec![
                Arc::new(GnmiGetConfigEnricher {
                    paths: config.default_gnmi_get_paths.clone(),
                }),
                Arc::new(ParserChainCliEnricher::new(config.parser_chain.clone())),
            ],
        }
    }

    /// Register an additional enricher at runtime (e.g., a REST-API or BMP enricher).
    pub fn register(&mut self, enricher: Arc<dyn MultiSourceEnricher>) {
        self.enrichers.push(enricher);
    }

    /// Attempt capture in priority order determined by `capture_plan`.
    /// Returns the first successful result; fails only if every enricher fails.
    pub async fn capture(
        &self,
        target: &TargetConfig,
        credentials: Option<&ResolvedCredential>,
        gnmi_readiness: Option<&GnmiReadinessReport>,
    ) -> Result<MultiSourceCapture> {
        let plan = self.capture_plan(target, gnmi_readiness);
        let mut failures = Vec::new();
        for enricher in plan {
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

    /// Determine enricher priority order for a target.
    ///
    /// Decision rules (in descending priority):
    /// 1. If the gNMI readiness report shows blockers or the service is not
    ///    reachable, deprioritise gNMI enrichers — prefer CLI enrichers first.
    /// 2. For vendors known to have poor gNMI ON_CHANGE coverage in lab (IOS-XRd,
    ///    JunOS, Arista EOS, FRR), when no live readiness report is available,
    ///    fall back to the vendor-heuristic (same as before).
    /// 3. Otherwise gNMI leads.
    fn capture_plan(
        &self,
        target: &TargetConfig,
        gnmi_readiness: Option<&GnmiReadinessReport>,
    ) -> Vec<Arc<dyn MultiSourceEnricher>> {
        let gnmi_blocked = gnmi_readiness
            .map(|r| !r.blockers.is_empty() || r.service_status != "reachable")
            .unwrap_or_else(|| vendor_prefers_cli_fallback(target));

        if gnmi_blocked {
            // CLI enrichers first, then gNMI as fallback
            let mut ordered: Vec<Arc<dyn MultiSourceEnricher>> = self
                .enrichers
                .iter()
                .filter(|e| e.capability_tags().contains(&"cli"))
                .cloned()
                .collect();
            ordered.extend(
                self.enrichers
                    .iter()
                    .filter(|e| !e.capability_tags().contains(&"cli"))
                    .cloned(),
            );
            ordered
        } else {
            // gNMI enrichers first, then CLI and others
            let mut ordered: Vec<Arc<dyn MultiSourceEnricher>> = self
                .enrichers
                .iter()
                .filter(|e| e.capability_tags().contains(&"gnmi"))
                .cloned()
                .collect();
            ordered.extend(
                self.enrichers
                    .iter()
                    .filter(|e| !e.capability_tags().contains(&"gnmi"))
                    .cloned(),
            );
            ordered
        }
    }
}

/// Static vendor heuristic when no live GnmiReadinessReport is available.
/// These vendors have historically poor gNMI coverage at lab scale without TLS.
fn vendor_prefers_cli_fallback(target: &TargetConfig) -> bool {
    use crate::enrichment::parser_chain_enricher::inferred_vendor;
    let vendor = inferred_vendor(target);
    matches!(
        vendor.as_str(),
        "cisco-iosxr" | "juniper-junos" | "arista-eos" | "frr"
    ) && target.ca_cert.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TargetConfig;

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

    fn registry() -> MultiSourceEnricherRegistry {
        MultiSourceEnricherRegistry {
            enrichers: vec![
                Arc::new(GnmiGetConfigEnricher { paths: vec![] }),
                Arc::new(ParserChainCliEnricher::new(Default::default())),
            ],
        }
    }

    #[test]
    fn gnmi_blocked_readiness_puts_cli_first() {
        let reg = registry();
        let t = target("nokia-srlinux", true);
        let report = GnmiReadinessReport {
            service_status: "unreachable".to_string(),
            tls_status: "unknown".to_string(),
            encoding_support: vec![],
            models_advertised: vec![],
            known_issues: vec![],
            blockers: vec!["gNMI port unreachable".to_string()],
            recommended_actions: vec![],
            checked_at_ns: 0,
        };
        let plan = reg.capture_plan(&t, Some(&report));
        assert!(plan[0].capability_tags().contains(&"cli"),
            "CLI enricher should lead when gNMI is blocked");
    }

    #[test]
    fn gnmi_ready_readiness_puts_gnmi_first() {
        let reg = registry();
        let t = target("nokia-srlinux", true);
        let report = GnmiReadinessReport {
            service_status: "reachable".to_string(),
            tls_status: "tls_ok".to_string(),
            encoding_support: vec!["JSON_IETF".to_string()],
            models_advertised: vec!["openconfig".to_string()],
            known_issues: vec![],
            blockers: vec![],
            recommended_actions: vec![],
            checked_at_ns: 0,
        };
        let plan = reg.capture_plan(&t, Some(&report));
        assert!(plan[0].capability_tags().contains(&"gnmi"),
            "gNMI enricher should lead when gNMI is reachable");
    }

    #[test]
    fn no_readiness_report_falls_back_to_vendor_heuristic() {
        let reg = registry();
        // IOS-XRd without TLS → CLI preferred
        let t = target("cisco-iosxr", false);
        let plan = reg.capture_plan(&t, None);
        assert!(plan[0].capability_tags().contains(&"cli"),
            "CLI enricher should lead for IOS-XRd without TLS when no readiness report");

        // Nokia SRL without TLS → gNMI preferred (not in vendor fallback list)
        let t2 = target("nokia-srlinux", false);
        let plan2 = reg.capture_plan(&t2, None);
        assert!(plan2[0].capability_tags().contains(&"gnmi"),
            "gNMI enricher should lead for Nokia SRL");
    }
}

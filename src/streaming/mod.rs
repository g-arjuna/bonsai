pub mod bgp_ls;
pub mod bmp;
pub mod netflow;
pub mod otlp;

use serde::{Deserialize, Serialize};

use crate::config::{StreamingConfig, TargetConfig};
use crate::discovery::GnmiReadinessReport;
use crate::graph::common::now_ns;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportLevel {
    Native,
    Conditional,
    Unsupported,
}

impl SupportLevel {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Conditional => "conditional",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolReadiness {
    pub protocol: String,
    pub support_level: String,
    pub configured: bool,
    pub status: String,
    pub blockers: Vec<String>,
    pub recommended_actions: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolRecommendation {
    pub protocol: String,
    pub priority: u8,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamingReadinessReport {
    pub device_address: String,
    pub vendor: String,
    pub role: String,
    pub checked_at_ns: i64,
    pub protocols: Vec<ProtocolReadiness>,
    pub recommended_protocols: Vec<ProtocolRecommendation>,
}

pub fn build_streaming_readiness_report(
    target: &TargetConfig,
    gnmi_report: Option<&GnmiReadinessReport>,
    cfg: &StreamingConfig,
) -> StreamingReadinessReport {
    let vendor = normalized_vendor(target);
    let role = target.role.clone().unwrap_or_default().to_lowercase();
    let is_bgp_role = role_supports_bgp_monitoring(&role);
    let is_te_role = role_supports_te_topology(&role);
    let is_sp_role = role_supports_pcep(&role);

    let mut protocols = Vec::new();
    let mut recommendations = Vec::new();

    let mut gnmi_notes =
        vec!["Uses OpenConfig/gNMI capabilities and live RPC reachability.".to_string()];
    if gnmi_report.is_none() {
        gnmi_notes.push(
            "No live gNMI check was run for this device; overall streaming readiness is inferred from static target metadata."
                .to_string(),
        );
    }
    let gnmi_readiness = if let Some(report) = gnmi_report {
        ProtocolReadiness {
            protocol: "gnmi".to_string(),
            support_level: SupportLevel::Native.as_str().to_string(),
            configured: target.ca_cert.is_some()
                || target.username.is_some()
                || target.credential_alias.is_some(),
            status: if report.blockers.is_empty() && report.service_status == "reachable" {
                "ready".to_string()
            } else {
                "degraded".to_string()
            },
            blockers: report.blockers.clone(),
            recommended_actions: report.recommended_actions.clone(),
            notes: gnmi_notes,
        }
    } else {
        ProtocolReadiness {
            protocol: "gnmi".to_string(),
            support_level: SupportLevel::Native.as_str().to_string(),
            configured: false,
            status: "unknown".to_string(),
            blockers: vec!["Live gNMI readiness has not been evaluated.".to_string()],
            recommended_actions: vec![
                "Run the device gNMI readiness check to verify TLS, credentials, and Capabilities RPC reachability."
                    .to_string(),
            ],
            notes: gnmi_notes,
        }
    };
    if gnmi_readiness.status == "ready" {
        recommendations.push(ProtocolRecommendation {
            protocol: "gnmi".to_string(),
            priority: 1,
            rationale: "gNMI is the primary device-state stream and should stay enabled wherever it is reachable.".to_string(),
        });
    }
    protocols.push(gnmi_readiness);

    let (bmp_support, bmp_note) = bmp_support_for_vendor(&vendor);
    let bmp_configured = cfg.bmp.enabled;
    let mut bmp_blockers = Vec::new();
    let mut bmp_actions = Vec::new();
    if !is_bgp_role {
        bmp_blockers.push("Device role does not normally originate or reflect BGP RIB state worth exporting over BMP.".to_string());
    }
    if !bmp_configured {
        bmp_actions.push(
            "Enable [streaming.bmp] and point the device at the Bonsai BMP listener.".to_string(),
        );
    }
    let bmp_status = protocol_status(
        bmp_support.clone(),
        bmp_configured,
        is_bgp_role,
        &bmp_blockers,
    );
    if bmp_status == "ready" {
        recommendations.push(ProtocolRecommendation {
            protocol: "bmp".to_string(),
            priority: 2,
            rationale: "BMP is the standards-based way to add per-prefix BGP visibility across vendors without polling device RIBs.".to_string(),
        });
    }
    protocols.push(ProtocolReadiness {
        protocol: "bmp".to_string(),
        support_level: bmp_support.as_str().to_string(),
        configured: bmp_configured,
        status: bmp_status.to_string(),
        blockers: bmp_blockers,
        recommended_actions: bmp_actions,
        notes: vec![bmp_note],
    });

    let (bgp_ls_support, bgp_ls_note) = bgp_ls_support_for_vendor(&vendor);
    let bgp_ls_configured = cfg.bgp_ls.enabled;
    let mut bgp_ls_blockers = Vec::new();
    let mut bgp_ls_actions = Vec::new();
    if !is_te_role {
        bgp_ls_blockers.push(
            "Role is not a typical TE topology participant; BGP-LS is most useful on spines, PE/P/RR nodes, and route reflectors."
                .to_string(),
        );
    }
    if !bgp_ls_configured {
        bgp_ls_actions.push(
            "Enable [streaming.bgp_ls] and connect a GoBGP sidecar or route reflector feed to the JSON listener."
                .to_string(),
        );
    }
    let bgp_ls_status = protocol_status(
        bgp_ls_support.clone(),
        bgp_ls_configured,
        is_te_role,
        &bgp_ls_blockers,
    );
    if bgp_ls_status == "ready" {
        recommendations.push(ProtocolRecommendation {
            protocol: "bgp_ls".to_string(),
            priority: 3,
            rationale:
                "BGP-LS gives Bonsai a vendor-neutral topology and TE feed without reconstructing global state from per-device telemetry."
                    .to_string(),
        });
    }
    protocols.push(ProtocolReadiness {
        protocol: "bgp_ls".to_string(),
        support_level: bgp_ls_support.as_str().to_string(),
        configured: bgp_ls_configured,
        status: bgp_ls_status.to_string(),
        blockers: bgp_ls_blockers,
        recommended_actions: bgp_ls_actions,
        notes: vec![bgp_ls_note],
    });

    let (pcep_support, pcep_note) = pcep_support_for_vendor(&vendor);
    let pcep_configured = cfg.pcep.enabled;
    let mut pcep_blockers =
        vec!["PCEP ingest is intentionally deferred until the SP lab is validated.".to_string()];
    let mut pcep_actions = vec![
        "Keep PCEP disabled for now; revisit once the service-provider lab and SR-PCE workflow are up."
            .to_string(),
    ];
    if !is_sp_role {
        pcep_blockers.push(
            "Role is not service-provider oriented; PCEP is typically relevant only for PCC/PCE nodes and SR-TE deployments."
                .to_string(),
        );
    }
    if !pcep_configured {
        pcep_actions.push(
            "When the SP lab is ready, enable [streaming.pcep] and point PCC/PCE sessions at the Bonsai collector."
                .to_string(),
        );
    }
    protocols.push(ProtocolReadiness {
        protocol: "pcep".to_string(),
        support_level: pcep_support.as_str().to_string(),
        configured: pcep_configured,
        status: "deferred".to_string(),
        blockers: pcep_blockers,
        recommended_actions: pcep_actions,
        notes: vec![pcep_note],
    });

    recommendations.sort_by_key(|item| item.priority);

    StreamingReadinessReport {
        device_address: target.address.clone(),
        vendor,
        role,
        checked_at_ns: now_ns(),
        protocols,
        recommended_protocols: recommendations,
    }
}

fn protocol_status(
    support: SupportLevel,
    configured: bool,
    role_match: bool,
    blockers: &[String],
) -> &'static str {
    if support == SupportLevel::Unsupported {
        return "unsupported";
    }
    if !configured {
        return "not_configured";
    }
    if !role_match || !blockers.is_empty() {
        return "degraded";
    }
    "ready"
}

fn normalized_vendor(target: &TargetConfig) -> String {
    target
        .vendor
        .clone()
        .or_else(|| infer_vendor_from_hostname(target.hostname.as_deref()))
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase()
}

fn infer_vendor_from_hostname(hostname: Option<&str>) -> Option<String> {
    let host = hostname?.to_lowercase();
    if host.starts_with("xrd") {
        Some("cisco_xrd".to_string())
    } else if host.starts_with("srl") {
        Some("nokia_srl".to_string())
    } else if host.starts_with("ceos") {
        Some("arista_ceos".to_string())
    } else if host.starts_with("crpd") || host.starts_with("vjunos") {
        Some("juniper_crpd".to_string())
    } else if host.starts_with("frr") || host.starts_with("holo") {
        Some("frr".to_string())
    } else {
        None
    }
}

fn role_supports_bgp_monitoring(role: &str) -> bool {
    matches!(
        role,
        "leaf" | "spine" | "super-spine" | "pe" | "p" | "rr" | "ce" | "core"
    )
}

fn role_supports_te_topology(role: &str) -> bool {
    matches!(
        role,
        "leaf" | "spine" | "super-spine" | "pe" | "p" | "rr" | "core"
    )
}

fn role_supports_pcep(role: &str) -> bool {
    matches!(role, "pe" | "p" | "rr" | "core")
}

fn bmp_support_for_vendor(vendor: &str) -> (SupportLevel, String) {
    let note = "BMP support is evaluated against RFC 7854-style export and assumes the device can source BMP to an external collector.".to_string();
    if matches_vendor(
        vendor,
        &[
            "nokia_srl",
            "cisco_xrd",
            "juniper_crpd",
            "arista_ceos",
            "frr",
        ],
    ) {
        (SupportLevel::Native, note)
    } else {
        (SupportLevel::Conditional, note)
    }
}

fn bgp_ls_support_for_vendor(vendor: &str) -> (SupportLevel, String) {
    let note = "BGP-LS support assumes a standards-based AFI/SAFI 16388/71 feed, usually relayed through a route reflector or GoBGP sidecar.".to_string();
    if matches_vendor(
        vendor,
        &["nokia_srl", "cisco_xrd", "juniper_crpd", "arista_ceos"],
    ) {
        (SupportLevel::Native, note)
    } else {
        (SupportLevel::Conditional, note)
    }
}

fn pcep_support_for_vendor(vendor: &str) -> (SupportLevel, String) {
    let note = "PCEP readiness is advisory only in this sprint because Bonsai does not yet run a production PCEP parser.".to_string();
    if matches_vendor(vendor, &["nokia_srl", "cisco_xrd", "juniper_crpd"]) {
        (SupportLevel::Conditional, note)
    } else {
        (SupportLevel::Unsupported, note)
    }
}

fn matches_vendor(vendor: &str, supported: &[&str]) -> bool {
    supported.iter().any(|item| vendor.contains(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_readiness_prefers_gnmi_then_bmp_for_bgp_roles() {
        let target = TargetConfig {
            address: "10.0.0.1:57400".to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some("cisco_xrd".to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: Some("xrd-pe1".to_string()),
            role: Some("pe".to_string()),
            site: None,
            collector_id: None,
            selected_paths: Vec::new(),
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
        };
        let gnmi = GnmiReadinessReport {
            service_status: "reachable".to_string(),
            tls_status: "disabled".to_string(),
            encoding_support: vec!["JSON_IETF".to_string()],
            models_advertised: vec!["Cisco-IOS-XR".to_string()],
            known_issues: Vec::new(),
            blockers: Vec::new(),
            recommended_actions: Vec::new(),
            checked_at_ns: 1,
        };
        let cfg = StreamingConfig {
            bmp: crate::config::BmpConfig {
                enabled: true,
                ..Default::default()
            },
            bgp_ls: crate::config::BgpLsConfig {
                enabled: true,
                ..Default::default()
            },
            pcep: crate::config::PcepConfig::default(),
        };

        let report = build_streaming_readiness_report(&target, Some(&gnmi), &cfg);
        assert_eq!(report.recommended_protocols[0].protocol, "gnmi");
        assert_eq!(report.recommended_protocols[1].protocol, "bmp");
        assert!(report.protocols.iter().any(|p| p.protocol == "bgp_ls"));
    }
}

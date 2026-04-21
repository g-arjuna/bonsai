use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{CapabilityRequest, Encoding, ModelData};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);
const PATH_PROFILE_DIR: &str = "config/path_profiles";
const SAMPLE_INTERVAL_10S: u64 = 10_000_000_000;
const SAMPLE_INTERVAL_60S: u64 = 60_000_000_000;

#[derive(Clone, Debug)]
pub struct DiscoveryInput {
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub username_env: Option<String>,
    pub password_env: Option<String>,
    pub ca_cert_path: Option<String>,
    pub tls_domain: Option<String>,
    pub role_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiscoveryReport {
    pub vendor_detected: String,
    pub models_advertised: Vec<String>,
    pub gnmi_encoding: String,
    pub recommended_profiles: Vec<PathProfileMatch>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PathProfileMatch {
    pub profile_name: String,
    pub paths: Vec<SubscriptionPath>,
    pub rationale: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubscriptionPath {
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
    pub rationale: String,
    pub optional: bool,
}

#[derive(Clone, Debug)]
struct CapabilitySummary {
    vendor_label: String,
    encoding: String,
    model_names: Vec<String>,
    has_oc_interfaces: bool,
    has_oc_bfd: bool,
    has_oc_bgp: bool,
    has_oc_lldp: bool,
    has_oc_mpls: bool,
    has_oc_segment_routing: bool,
    has_oc_isis: bool,
    has_srl_native: bool,
    has_srl_native_bfd: bool,
    has_xr_native: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct PathProfileTemplate {
    name: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    description: String,
    rationale: String,
    #[serde(default)]
    paths: Vec<PathTemplate>,
}

#[derive(Clone, Debug, Deserialize)]
struct PathTemplate {
    path: String,
    #[serde(default)]
    origin: String,
    mode: String,
    #[serde(default)]
    sample_interval_ns: u64,
    #[serde(default)]
    required_models: Vec<String>,
    #[serde(default)]
    required_any_models: Vec<String>,
    #[serde(default)]
    optional: bool,
    rationale: String,
}

pub async fn discover_device(input: DiscoveryInput) -> Result<DiscoveryReport> {
    let address = normalize_required("address", input.address)?;
    let username = input.username.or(env_value(input.username_env.as_deref())?);
    let password = input.password.or(env_value(input.password_env.as_deref())?);
    let channel = connect(
        &address,
        input.ca_cert_path.as_deref(),
        input.tls_domain.as_deref(),
    )
    .await?;

    let mut client = GNmiClient::new(channel);
    let mut request = Request::new(CapabilityRequest::default());
    add_auth_metadata(&mut request, username.as_deref(), password.as_deref());

    let response = tokio::time::timeout(DISCOVERY_TIMEOUT, client.capabilities(request))
        .await
        .with_context(|| format!("Capabilities RPC timed out for {address}"))?
        .with_context(|| format!("Capabilities RPC failed for {address}"))?
        .into_inner();

    let summary =
        CapabilitySummary::from_response(&response.supported_models, &response.supported_encodings);
    let mut warnings = discovery_warnings(&summary, input.role_hint.as_deref());
    if input.ca_cert_path.is_some() && input.tls_domain.as_deref().unwrap_or_default().is_empty() {
        warnings.push(
            "TLS CA cert was provided without tls_domain; SNI may fail on some targets".to_string(),
        );
    }
    let (recommended_profiles, profile_warnings) =
        recommend_profiles(&summary, input.role_hint.as_deref());
    warnings.extend(profile_warnings);

    Ok(DiscoveryReport {
        vendor_detected: summary.vendor_label.clone(),
        models_advertised: summary.model_names.clone(),
        gnmi_encoding: summary.encoding.clone(),
        recommended_profiles,
        warnings,
    })
}

impl CapabilitySummary {
    fn from_response(models: &[ModelData], encodings: &[i32]) -> Self {
        let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
        Self::from_model_names(model_names, encodings)
    }

    fn from_model_names(model_names: Vec<String>, encodings: &[i32]) -> Self {
        let has_oc_interfaces = model_names.iter().any(|m| m == "openconfig-interfaces");
        let has_oc_bfd = model_names.iter().any(|m| m == "openconfig-bfd");
        let has_oc_bgp = model_names
            .iter()
            .any(|m| m == "openconfig-bgp" || m == "openconfig-network-instance");
        let has_oc_lldp = model_names.iter().any(|m| m == "openconfig-lldp");
        let has_oc_mpls = model_names.iter().any(|m| m == "openconfig-mpls");
        let has_oc_segment_routing = model_names
            .iter()
            .any(|m| m == "openconfig-segment-routing");
        let has_oc_isis = model_names.iter().any(|m| m == "openconfig-isis");
        let has_srl_native = model_names.iter().any(|m| m.contains("srl_nokia"));
        let has_srl_native_bfd = model_names.iter().any(|m| m.contains("srl_nokia-bfd"));
        let has_xr_native = model_names
            .iter()
            .any(|m| m.contains("Cisco-IOS-XR-infra-statsd-oper"));

        let vendor_label = if has_srl_native {
            "nokia_srl"
        } else if model_names.iter().any(|m| m.starts_with("Cisco-IOS-XR")) {
            "cisco_xrd"
        } else if model_names
            .iter()
            .any(|m| m.to_lowercase().contains("arista") || m.contains("EOS"))
        {
            "arista_ceos"
        } else if model_names
            .iter()
            .any(|m| m.to_lowercase().starts_with("junos"))
        {
            "juniper_crpd"
        } else {
            "openconfig"
        }
        .to_string();

        Self {
            vendor_label,
            encoding: preferred_encoding(encodings),
            model_names,
            has_oc_interfaces,
            has_oc_bfd,
            has_oc_bgp,
            has_oc_lldp,
            has_oc_mpls,
            has_oc_segment_routing,
            has_oc_isis,
            has_srl_native,
            has_srl_native_bfd,
            has_xr_native,
        }
    }
}

fn recommend_profiles(
    summary: &CapabilitySummary,
    role_hint: Option<&str>,
) -> (Vec<PathProfileMatch>, Vec<String>) {
    match load_path_profiles(PATH_PROFILE_DIR)
        .and_then(|profiles| recommend_profiles_from_templates(summary, role_hint, &profiles))
    {
        Ok(result) => result,
        Err(error) => {
            let mut warnings = vec![format!(
                "failed to load path profile templates from {PATH_PROFILE_DIR}: {error:#}; using built-in fallback"
            )];
            let profiles = recommend_profiles_builtin(summary, role_hint);
            if profiles.iter().all(|profile| profile.paths.is_empty()) {
                warnings.push("built-in fallback produced no subscribable paths".to_string());
            }
            (profiles, warnings)
        }
    }
}

fn recommend_profiles_builtin(
    summary: &CapabilitySummary,
    role_hint: Option<&str>,
) -> Vec<PathProfileMatch> {
    let profile_name = profile_name_for_role(role_hint);
    let mut paths = base_paths(summary);
    let mut expected = vec![
        summary.has_srl_native || summary.has_xr_native || summary.has_oc_interfaces,
        summary.has_srl_native || summary.has_oc_bgp,
        summary.has_srl_native || summary.has_xr_native || summary.has_oc_lldp,
    ];

    if profile_name.starts_with("sp_") {
        add_if_supported(
            &mut paths,
            summary.has_oc_mpls,
            SubscriptionPath {
                path: "mpls".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
                rationale: "SP profile requested and openconfig-mpls is advertised".to_string(),
                optional: false,
            },
        );
        add_if_supported(
            &mut paths,
            summary.has_oc_segment_routing,
            SubscriptionPath {
                path: "segment-routing".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
                rationale: "SP profile requested and openconfig-segment-routing is advertised"
                    .to_string(),
                optional: false,
            },
        );
        add_if_supported(
            &mut paths,
            summary.has_oc_isis,
            SubscriptionPath {
                path: "network-instances/network-instance/protocols/protocol/isis".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
                rationale: "SP profile requested and openconfig-isis is advertised".to_string(),
                optional: false,
            },
        );
        expected.extend([
            summary.has_oc_mpls,
            summary.has_oc_segment_routing,
            summary.has_oc_isis,
        ]);
    }

    let supported = expected.iter().filter(|supported| **supported).count() as f32;
    let confidence = if expected.is_empty() {
        0.0
    } else {
        supported / expected.len() as f32
    };

    vec![PathProfileMatch {
        profile_name: profile_name.to_string(),
        paths,
        rationale: format!(
            "matched role '{}' against advertised gNMI models and current built-in path rules",
            role_hint.unwrap_or("leaf")
        ),
        confidence,
    }]
}

fn recommend_profiles_from_templates(
    summary: &CapabilitySummary,
    role_hint: Option<&str>,
    templates: &[PathProfileTemplate],
) -> Result<(Vec<PathProfileMatch>, Vec<String>)> {
    let role = canonical_role(role_hint);
    let selected: Vec<&PathProfileTemplate> = templates
        .iter()
        .filter(|template| {
            template
                .roles
                .iter()
                .any(|profile_role| profile_role.eq_ignore_ascii_case(&role))
        })
        .collect();

    if selected.is_empty() {
        bail!("no path profile template matched role '{role}'");
    }

    let mut warnings = Vec::new();
    let mut matches = Vec::new();
    for template in selected {
        let (paths, dropped, total) = supported_template_paths(summary, template);
        for dropped_path in dropped {
            warnings.push(format!(
                "profile '{}' dropped path '{}': {}",
                template.name, dropped_path.path, dropped_path.reason
            ));
        }
        let confidence = if total == 0 {
            0.0
        } else {
            paths.len() as f32 / total as f32
        };
        matches.push(PathProfileMatch {
            profile_name: template.name.clone(),
            paths,
            rationale: if template.description.is_empty() {
                template.rationale.clone()
            } else {
                format!("{} {}", template.description, template.rationale)
            },
            confidence,
        });
    }

    Ok((matches, warnings))
}

struct DroppedPath {
    path: String,
    reason: String,
}

fn supported_template_paths(
    summary: &CapabilitySummary,
    template: &PathProfileTemplate,
) -> (Vec<SubscriptionPath>, Vec<DroppedPath>, usize) {
    let mut supported = Vec::new();
    let mut dropped = Vec::new();

    for path in &template.paths {
        match missing_requirements(summary, path) {
            None => supported.push(SubscriptionPath {
                path: path.path.clone(),
                origin: path.origin.clone(),
                mode: path.mode.clone(),
                sample_interval_ns: path.sample_interval_ns,
                rationale: path.rationale.clone(),
                optional: path.optional,
            }),
            Some(reason) => {
                let kind = if path.optional {
                    "optional"
                } else {
                    "required"
                };
                dropped.push(DroppedPath {
                    path: path.path.clone(),
                    reason: format!("{kind} requirement not met ({reason})"),
                });
            }
        }
    }

    (supported, dropped, template.paths.len())
}

fn missing_requirements(summary: &CapabilitySummary, path: &PathTemplate) -> Option<String> {
    let missing_required: Vec<String> = path
        .required_models
        .iter()
        .filter(|required| !summary.has_model(required))
        .cloned()
        .collect();
    if !missing_required.is_empty() {
        return Some(format!("missing all required models: {missing_required:?}"));
    }

    if !path.required_any_models.is_empty()
        && !path
            .required_any_models
            .iter()
            .any(|required| summary.has_model(required))
    {
        return Some(format!(
            "missing any of required models: {:?}",
            path.required_any_models
        ));
    }

    None
}

impl CapabilitySummary {
    fn has_model(&self, required: &str) -> bool {
        self.model_names
            .iter()
            .any(|model| model == required || model.contains(required))
    }
}

fn load_path_profiles(path: impl AsRef<std::path::Path>) -> Result<Vec<PathProfileTemplate>> {
    let path = path.as_ref();
    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("could not read path profile directory '{}'", path.display()))?
    {
        let entry = entry?;
        let file_path = entry.path();
        let is_yaml = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"));
        if !is_yaml {
            continue;
        }
        let raw = std::fs::read_to_string(&file_path)
            .with_context(|| format!("could not read path profile '{}'", file_path.display()))?;
        let profile: PathProfileTemplate = serde_yaml::from_str(&raw)
            .with_context(|| format!("could not parse path profile '{}'", file_path.display()))?;
        profiles.push(profile);
    }

    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    if profiles.is_empty() {
        bail!("no .yaml path profiles found in '{}'", path.display());
    }
    Ok(profiles)
}

fn base_paths(summary: &CapabilitySummary) -> Vec<SubscriptionPath> {
    let mut paths = Vec::new();

    if summary.has_srl_native {
        paths.push(sample(
            "interface[name=*]/statistics",
            "",
            SAMPLE_INTERVAL_10S,
            "SR Linux native interface counters advertised",
        ));
        paths.push(on_change(
            "interface[name=*]/oper-state",
            "",
            "SR Linux native interface oper-state advertised",
        ));
        paths.push(on_change(
            "network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]",
            "",
            "SR Linux native BGP model advertised",
        ));
        paths.push(on_change(
            "system/lldp/interface[name=*]/neighbor[id=*]",
            "",
            "SR Linux native LLDP model advertised",
        ));
        if summary.has_srl_native_bfd {
            paths.push(on_change(
                "bfd/network-instance[name=default]",
                "",
                "SR Linux native BFD model advertised",
            ));
        }
    } else {
        if summary.has_xr_native {
            paths.push(sample(
                "Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters",
                "",
                SAMPLE_INTERVAL_10S,
                "IOS-XR native interface counters advertised",
            ));
            paths.push(sample(
                "Cisco-IOS-XR-ethernet-lldp-oper:lldp/nodes/node/neighbors/details/detail",
                "",
                SAMPLE_INTERVAL_60S,
                "IOS-XR native LLDP path is sampled so existing neighbors are observed",
            ));
        }
        if summary.has_oc_interfaces {
            paths.push(sample(
                "interfaces",
                "openconfig",
                SAMPLE_INTERVAL_10S,
                "openconfig-interfaces is advertised",
            ));
            paths.push(on_change(
                "interfaces",
                "openconfig",
                "openconfig-interfaces carries oper status updates",
            ));
        }
        if summary.has_oc_bgp {
            paths.push(on_change(
                "network-instances",
                "openconfig",
                "openconfig-bgp or openconfig-network-instance is advertised",
            ));
        }
        if summary.has_oc_bfd {
            paths.push(on_change(
                "bfd",
                "openconfig",
                "openconfig-bfd is advertised",
            ));
        }
        if summary.has_oc_lldp {
            paths.push(on_change(
                "lldp",
                "openconfig",
                "openconfig-lldp is advertised",
            ));
        }
    }

    paths
}

fn discovery_warnings(summary: &CapabilitySummary, role_hint: Option<&str>) -> Vec<String> {
    let mut warnings = Vec::new();
    let raw_role = role_hint.unwrap_or("leaf").trim();
    let normalized_role = raw_role.to_lowercase();
    if !matches!(
        normalized_role.as_str(),
        "" | "leaf" | "spine" | "pe" | "p" | "rr"
    ) {
        warnings.push(format!(
            "unknown role_hint '{}'; using dc_leaf_minimal recommendations",
            raw_role
        ));
    }
    if summary.model_names.is_empty() {
        warnings.push("device returned no advertised models".to_string());
    }
    if !summary.has_srl_native && !summary.has_xr_native && !summary.has_oc_interfaces {
        warnings.push(
            "no interface model was advertised; interface telemetry may be unavailable".to_string(),
        );
    }
    if !summary.has_srl_native && !summary.has_oc_bgp {
        warnings.push("no BGP model was advertised; BGP telemetry may be unavailable".to_string());
    }
    if !summary.has_srl_native && !summary.has_xr_native && !summary.has_oc_lldp {
        warnings
            .push("no LLDP model was advertised; topology discovery may be incomplete".to_string());
    }
    warnings
}

fn profile_name_for_role(role_hint: Option<&str>) -> &'static str {
    match canonical_role(role_hint).as_str() {
        "spine" => "dc_spine_standard",
        "pe" | "rr" => "sp_pe_full",
        "p" => "sp_p_core",
        _ => "dc_leaf_minimal",
    }
}

fn canonical_role(role_hint: Option<&str>) -> String {
    match role_hint.unwrap_or("leaf").trim().to_lowercase().as_str() {
        "" | "leaf" => "leaf".to_string(),
        "spine" => "spine".to_string(),
        "pe" => "pe".to_string(),
        "p" => "p".to_string(),
        "rr" => "rr".to_string(),
        _ => "leaf".to_string(),
    }
}

fn add_if_supported(paths: &mut Vec<SubscriptionPath>, supported: bool, path: SubscriptionPath) {
    if supported {
        paths.push(path);
    }
}

fn sample(path: &str, origin: &str, interval: u64, rationale: &str) -> SubscriptionPath {
    SubscriptionPath {
        path: path.to_string(),
        origin: origin.to_string(),
        mode: "SAMPLE".to_string(),
        sample_interval_ns: interval,
        rationale: rationale.to_string(),
        optional: false,
    }
}

fn on_change(path: &str, origin: &str, rationale: &str) -> SubscriptionPath {
    SubscriptionPath {
        path: path.to_string(),
        origin: origin.to_string(),
        mode: "ON_CHANGE".to_string(),
        sample_interval_ns: 0,
        rationale: rationale.to_string(),
        optional: false,
    }
}

async fn connect(
    address: &str,
    ca_cert_path: Option<&str>,
    tls_domain: Option<&str>,
) -> Result<Channel> {
    let use_tls = ca_cert_path.is_some();
    let scheme = if use_tls { "https" } else { "http" };
    let endpoint = format!("{scheme}://{address}");

    let mut builder = Channel::from_shared(endpoint.clone())
        .context("invalid gNMI endpoint")?
        .timeout(DISCOVERY_TIMEOUT);

    if let Some(path) = ca_cert_path {
        let cert_pem = tokio::fs::read(path)
            .await
            .with_context(|| format!("could not read CA cert from '{path}'"))?;
        let domain = tls_domain.unwrap_or_default().trim();
        if domain.is_empty() {
            bail!("tls_domain is required when ca_cert_path is provided");
        }
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(cert_pem))
            .domain_name(domain.to_string());
        builder = builder.tls_config(tls).context("TLS config failed")?;
    }

    builder
        .connect()
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))
}

fn add_auth_metadata(
    request: &mut Request<CapabilityRequest>,
    username: Option<&str>,
    password: Option<&str>,
) {
    if let Some(username) = username
        && let Ok(value) = MetadataValue::try_from(username)
    {
        request.metadata_mut().insert("username", value);
    }
    if let Some(password) = password
        && let Ok(value) = MetadataValue::try_from(password)
    {
        request.metadata_mut().insert("password", value);
    }
}

fn env_value(env_name: Option<&str>) -> Result<Option<String>> {
    let Some(env_name) = env_name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    std::env::var(env_name)
        .with_context(|| format!("required credential env var '{env_name}' is not set"))
        .map(Some)
}

fn normalize_required(field: &str, value: String) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field} cannot be empty");
    }
    Ok(trimmed.to_string())
}

fn preferred_encoding(encodings: &[i32]) -> String {
    let json_ietf = Encoding::JsonIetf as i32;
    let json = Encoding::Json as i32;
    let preferred = if encodings.contains(&json_ietf) {
        Some(json_ietf)
    } else if encodings.contains(&json) {
        Some(json)
    } else {
        encodings.first().copied()
    };

    preferred
        .and_then(|encoding| Encoding::try_from(encoding).ok())
        .map(|encoding| encoding.as_str_name().to_string())
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srl_leaf_recommendation_uses_native_paths() {
        let summary = CapabilitySummary::from_model_names(
            vec![
                "urn:nokia.com:srlinux:chassis:srl_nokia-interfaces".to_string(),
                "urn:nokia.com:srlinux:network-instance:srl_nokia-bgp".to_string(),
                "urn:nokia.com:srlinux:system:srl_nokia-lldp".to_string(),
                "urn:nokia.com:srlinux:bfd:srl_nokia-bfd".to_string(),
            ],
            &[Encoding::JsonIetf as i32],
        );

        let profiles = recommend_profiles_builtin(&summary, Some("leaf"));
        assert_eq!(profiles[0].profile_name, "dc_leaf_minimal");
        assert!(profiles[0].confidence > 0.9);
        assert!(
            profiles[0]
                .paths
                .iter()
                .any(|path| path.path == "interface[name=*]/statistics")
        );
        assert!(
            !profiles[0]
                .paths
                .iter()
                .any(|path| path.path.contains("srl_nokia"))
        );
    }

    #[test]
    fn sp_profile_includes_only_advertised_sp_paths() {
        let summary = CapabilitySummary::from_model_names(
            vec![
                "openconfig-interfaces".to_string(),
                "openconfig-network-instance".to_string(),
                "openconfig-lldp".to_string(),
                "openconfig-mpls".to_string(),
            ],
            &[Encoding::JsonIetf as i32],
        );

        let profiles = recommend_profiles_builtin(&summary, Some("pe"));
        assert_eq!(profiles[0].profile_name, "sp_pe_full");
        assert!(profiles[0].paths.iter().any(|path| path.path == "mpls"));
        assert!(
            !profiles[0]
                .paths
                .iter()
                .any(|path| path.path == "segment-routing")
        );
        assert!(profiles[0].confidence > 0.4 && profiles[0].confidence < 0.8);
    }

    #[test]
    fn template_recommendation_filters_unsupported_paths() {
        let summary = CapabilitySummary::from_model_names(
            vec![
                "openconfig-interfaces".to_string(),
                "openconfig-network-instance".to_string(),
                "openconfig-lldp".to_string(),
                "openconfig-mpls".to_string(),
            ],
            &[Encoding::JsonIetf as i32],
        );
        let templates = load_path_profiles("config/path_profiles").expect("load path profiles");

        let (profiles, warnings) =
            recommend_profiles_from_templates(&summary, Some("pe"), &templates)
                .expect("recommend from templates");

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile_name, "sp_pe_full");
        assert!(profiles[0].paths.iter().any(|path| path.path == "mpls"));
        assert!(
            !profiles[0]
                .paths
                .iter()
                .any(|path| path.path == "segment-routing")
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("segment-routing"))
        );
    }
}

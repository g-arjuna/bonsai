use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

use crate::catalogue::{
    CataloguePath, CatalogueProfile, canonical_role, is_sp_role, load_catalogue,
};
use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{CapabilityRequest, Encoding, ModelData};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);
const PATH_PROFILE_DIR: &str = "config/path_profiles";

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
    /// Caller-supplied environment archetype (e.g. "data_center", "service_provider").
    /// When provided, overrides the role-inferred fallback so profile selection uses
    /// the device's actual environment rather than a guess from its role.
    pub environment_archetype: Option<String>,
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
}

type PathProfileTemplate = CatalogueProfile;
type PathTemplate = CataloguePath;

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
        recommend_profiles(&summary, input.role_hint.as_deref(), input.environment_archetype.as_deref());
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
        let has_srl_native = model_names.iter().any(|m| m.contains("srl_nokia"));

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
        }
    }

    fn has_model(&self, required: &str) -> bool {
        self.model_names
            .iter()
            .any(|model| model == required || model.contains(required))
    }
}

fn recommend_profiles(
    summary: &CapabilitySummary,
    role_hint: Option<&str>,
    environment_archetype: Option<&str>,
) -> (Vec<PathProfileMatch>, Vec<String>) {
    match load_templates_with_resolution(PATH_PROFILE_DIR)
        .and_then(|(profiles, mut warnings)| {
            let (matches, mut selection_warnings) =
                recommend_profiles_from_templates(summary, role_hint, environment_archetype, &profiles)?;
            warnings.append(&mut selection_warnings);
            Ok((matches, warnings))
        })
    {
        Ok(result) => result,
        Err(error) => {
            let warnings = vec![format!(
                "failed to load path profile templates from {PATH_PROFILE_DIR}: {error:#}"
            )];
            (Vec::new(), warnings)
        }
    }
}

fn load_templates_with_resolution(base_dir: &str) -> Result<(Vec<CatalogueProfile>, Vec<String>)> {
    let state = load_catalogue(std::path::Path::new(base_dir));
    let profiles = state.all_profiles().cloned().collect();
    let mut warnings = state.load_errors.clone();
    for plugin in &state.plugins {
        warnings.extend(plugin.conflicts.clone());
    }
    Ok((profiles, warnings))
}

fn recommend_profiles_from_templates(
    summary: &CapabilitySummary,
    role_hint: Option<&str>,
    environment_archetype: Option<&str>,
    templates: &[PathProfileTemplate],
) -> Result<(Vec<PathProfileMatch>, Vec<String>)> {
    let role = canonical_role(role_hint);
    let environment = environment_archetype.unwrap_or_else(|| inferred_environment_for_role(&role));
    let vendor = summary.vendor_label.as_str();
    let selected: Vec<&PathProfileTemplate> = templates
        .iter()
        .filter(|template| {
            profile_matches_environment(template, environment)
                && profile_matches_vendor_scope(template, vendor).is_some()
                && template
                    .roles
                    .iter()
                    .any(|profile_role| profile_role.eq_ignore_ascii_case(&role))
        })
        .collect();

    if selected.is_empty() {
        bail!(
            "no path profile template matched role '{role}', environment '{environment}', vendor '{vendor}'"
        );
    }

    let mut warnings = Vec::new();
    let mut scored_matches = Vec::new();
    for template in selected {
        let (paths, dropped, total) = supported_template_paths(summary, vendor, template);
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
        let vendor_exact = profile_matches_vendor_scope(template, vendor).unwrap_or(false);
        scored_matches.push((
            vendor_exact,
            PathProfileMatch {
                profile_name: template.name.clone(),
                paths,
                rationale: if template.description.is_empty() {
                    template.rationale.clone()
                } else {
                    format!("{} {}", template.description, template.rationale)
                },
                confidence,
            },
        ));
    }

    scored_matches.sort_by(|(a_exact, a_profile), (b_exact, b_profile)| {
        b_exact
            .cmp(a_exact)
            .then_with(|| b_profile.confidence.total_cmp(&a_profile.confidence))
            .then_with(|| b_profile.paths.len().cmp(&a_profile.paths.len()))
            .then_with(|| a_profile.profile_name.cmp(&b_profile.profile_name))
    });

    Ok((
        scored_matches
            .into_iter()
            .map(|(_, profile)| profile)
            .collect(),
        warnings,
    ))
}

fn profile_matches_environment(template: &PathProfileTemplate, environment: &str) -> bool {
    template.environment.is_empty()
        || template
            .environment
            .iter()
            .any(|env| env.eq_ignore_ascii_case(environment) || env.eq_ignore_ascii_case("any"))
}

fn profile_matches_vendor_scope(template: &PathProfileTemplate, vendor: &str) -> Option<bool> {
    if template.vendor_scope.is_empty() {
        return Some(false);
    }
    let has_exact = template
        .vendor_scope
        .iter()
        .any(|item| item.eq_ignore_ascii_case(vendor));
    let has_any = template
        .vendor_scope
        .iter()
        .any(|item| item.eq_ignore_ascii_case("any"));
    if has_exact {
        Some(true)
    } else if has_any {
        Some(false)
    } else {
        None
    }
}

fn inferred_environment_for_role(role: &str) -> &'static str {
    if is_sp_role(role) {
        "service_provider"
    } else {
        "data_center"
    }
}

struct DroppedPath {
    path: String,
    reason: String,
}

fn supported_template_paths(
    summary: &CapabilitySummary,
    vendor: &str,
    template: &PathProfileTemplate,
) -> (Vec<SubscriptionPath>, Vec<DroppedPath>, usize) {
    let mut dropped = Vec::new();
    let mut immediate_paths = Vec::new();
    let mut fallback_candidates = Vec::new();
    let mut eligible_paths = std::collections::HashSet::new();

    for path in &template.paths {
        match missing_requirements(summary, vendor, path) {
            None => {
                eligible_paths.insert(path.path.clone());
                let subscription = SubscriptionPath {
                    path: path.path.clone(),
                    origin: path.origin.clone(),
                    mode: path.mode.clone(),
                    sample_interval_ns: path.sample_interval_ns,
                    rationale: path.rationale.clone(),
                    optional: path.optional,
                };
                if path.fallback_for.is_some() {
                    fallback_candidates.push((subscription, path.fallback_for.clone(), path.optional));
                } else {
                    immediate_paths.push(subscription);
                }
            }
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

    let mut selected_paths: std::collections::HashSet<String> =
        immediate_paths.iter().map(|path| path.path.clone()).collect();
    for (fallback_path, fallback_for, optional) in fallback_candidates {
        if let Some(primary_path) = fallback_for
            && (eligible_paths.contains(&primary_path) || selected_paths.contains(&primary_path))
        {
            dropped.push(DroppedPath {
                path: fallback_path.path,
                reason: format!(
                    "{} fallback not selected because primary path '{}' is available",
                    if optional { "optional" } else { "required" },
                    primary_path
                ),
            });
            continue;
        }
        selected_paths.insert(fallback_path.path.clone());
        immediate_paths.push(fallback_path);
    }

    (immediate_paths, dropped, template.paths.len())
}

fn missing_requirements(summary: &CapabilitySummary, vendor: &str, path: &PathTemplate) -> Option<String> {
    if !path.vendor_only.is_empty() && !path.vendor_only.iter().any(|v| v.eq_ignore_ascii_case(vendor)) {
        return Some(format!("path is restricted to vendors {:?}", path.vendor_only));
    }

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

const KNOWN_ROLES: &[&str] = &[
    "leaf", "access", "spine", "superspine", "distribution", "border", "ce-facing",
    "pe", "p", "rr", "peering", "core", "edge", "router", "switch",
];

fn discovery_warnings(summary: &CapabilitySummary, role_hint: Option<&str>) -> Vec<String> {
    let mut warnings = Vec::new();
    let raw_role = role_hint.unwrap_or("leaf").trim();
    let normalized_role = raw_role.to_lowercase();
    if !normalized_role.is_empty()
        && !KNOWN_ROLES.contains(&normalized_role.as_str())
    {
        warnings.push(format!(
            "unknown role_hint '{}'",
            raw_role
        ));
    }
    if summary.model_names.is_empty() {
        warnings.push("device returned no advertised models".to_string());
    }
    warnings
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
    fn capability_summary_detects_vendor() {
        let summary = CapabilitySummary::from_model_names(
            vec![
                "urn:nokia.com:srlinux:chassis:srl_nokia-interfaces".to_string(),
            ],
            &[Encoding::JsonIetf as i32],
        );
        assert_eq!(summary.vendor_label, "nokia_srl");
    }
}


pub fn resolve_subscription_paths(
    device: &crate::config::TargetConfig,
    overrides: &[crate::registry::PathOverride],
) -> (Vec<crate::config::SelectedSubscriptionPath>, Vec<String>) {
    let mut audit = Vec::new();
    let mut paths: std::collections::BTreeMap<String, crate::config::SelectedSubscriptionPath> = std::collections::BTreeMap::new();
    
    // 1. Base profile paths
    if device.selected_paths.is_empty() {
        audit.push("Started with 0 paths (no base profile selected)".to_string());
    } else {
        audit.push(format!("Started from profile with {} paths", device.selected_paths.len()));
        for p in &device.selected_paths {
            paths.insert(p.path.clone(), p.clone());
        }
    }
    
    // Sort overrides correctly or apply them in order
    // Apply Role-Env overrides
    let role = device.role.as_deref().unwrap_or("");
    let _env = device.site.as_deref().unwrap_or("data_center"); // or look up actual env
    // Actually we need the environment, but the environment might be fetched from site or inferred.
    
    for ovr in overrides {
        let matches = match &ovr.scope {
            crate::registry::OverrideScope::Site(s) => device.site.as_deref() == Some(s.as_str()),
            crate::registry::OverrideScope::RoleEnv { role: r, environment: _e } => {
                // Here we would ideally check actual environment, for now just matching role as a best effort
                // In production, we'd pass environment to this function.
                role == r.as_str()
            },
            crate::registry::OverrideScope::Device(d) => device.address == d.as_str(),
        };
        
        if matches {
            let scope_str = match &ovr.scope {
                crate::registry::OverrideScope::Site(s) => format!("site-override({})", s),
                crate::registry::OverrideScope::RoleEnv { role: r, environment: e } => format!("role-override({}/{})", r, e),
                crate::registry::OverrideScope::Device(d) => format!("device-override({})", d),
            };
            
            match ovr.action {
                crate::registry::OverrideAction::Add => {
                    audit.push(format!("Applied {}: added path '{}'", scope_str, ovr.path));
                    let p = crate::config::SelectedSubscriptionPath {
                        path: ovr.path.clone(),
                        origin: format!("override: {}", scope_str),
                        mode: "SAMPLE".to_string(),
                        sample_interval_ns: ovr.sample_interval_s.unwrap_or(10) * 1_000_000_000,
                        rationale: "Added by override".to_string(),
                        optional: ovr.optional.unwrap_or(false),
                    };
                    paths.insert(ovr.path.clone(), p);
                }
                crate::registry::OverrideAction::Drop => {
                    if paths.remove(&ovr.path).is_some() {
                        audit.push(format!("Applied {}: dropped path '{}'", scope_str, ovr.path));
                    }
                }
                crate::registry::OverrideAction::Modify => {
                    if let Some(p) = paths.get_mut(&ovr.path) {
                        audit.push(format!("Applied {}: modified path '{}'", scope_str, ovr.path));
                        p.origin = format!("override(mod): {}", scope_str);
                        if let Some(s) = ovr.sample_interval_s {
                            p.sample_interval_ns = s * 1_000_000_000;
                        }
                        if let Some(opt) = ovr.optional {
                            p.optional = opt;
                        }
                    }
                }
            }
        }
    }
    
    audit.push(format!("Final path list has {} paths", paths.len()));
    (paths.into_values().collect(), audit)
}

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use tokio::process::Command;

use crate::config::TargetConfig;
use crate::credentials::ResolvedCredential;
use crate::enrichment::multi_source::{MultiSourceCapture, MultiSourceEnricher};
use crate::parser_chain::{ParseRequest, ParserChain};

const DEFAULT_SSH_PORT: u16 = 22;
const CLI_CAPTURE_SCRIPT: &str = "scripts/cli_capture.py";

pub struct ParserChainCliEnricher {
    parser_chain: ParserChain,
}

impl ParserChainCliEnricher {
    pub fn new(config: crate::config::ParserChainConfig) -> Self {
        Self {
            parser_chain: ParserChain::new(config),
        }
    }
}

#[async_trait::async_trait]
impl MultiSourceEnricher for ParserChainCliEnricher {
    fn name(&self) -> &str {
        "parser_chain_cli"
    }

    fn capability_tags(&self) -> Vec<&'static str> {
        vec!["cli"]
    }

    async fn capture(
        &self,
        target: &TargetConfig,
        credentials: Option<&ResolvedCredential>,
    ) -> Result<MultiSourceCapture> {
        let creds = credentials.ok_or_else(|| {
            anyhow!(
                "parser-chain CLI capture requires credentials for {}",
                target.address
            )
        })?;
        let vendor = inferred_vendor(target);
        let command_pattern = command_for_vendor(&vendor).ok_or_else(|| {
            anyhow!(
                "no parser-chain CLI command is defined for vendor '{}' on {}",
                vendor,
                target.address
            )
        })?;
        let ssh_target = ssh_target(target)?;
        let raw_output = capture_raw_output(&ssh_target, creds, command_pattern).await?;
        let decision = self
            .parser_chain
            .parse(ParseRequest {
                vendor: vendor.clone(),
                command_pattern: command_pattern.to_string(),
                raw_output,
            })
            .await?;
        let payload = serde_json::to_string_pretty(&decision.parsed_json)
            .context("failed to serialize parser-chain output")?;
        // Parser quality describes output *format*, not observation trust:
        // a structured parser result does not authenticate the device, prove
        // capture freshness/completeness, or establish semantic correctness.
        // Confidence therefore stays conservative — structured parses cap at
        // "medium" and degraded/unknown output at "low". BN-024-02 formalizes
        // the wider trust dimensions (source, completeness, freshness,
        // corroboration, authority eligibility).
        let confidence = match decision.quality.as_str() {
            crate::parser_chain::QUALITY_STRUCTURED => "medium",
            _ => "low",
        };
        // Serialize attempt diagnostics without duplicating full parsed
        // payloads: identity, outcome, quality and content digest/size are
        // sufficient provenance; the selected payload is stored once via
        // `capture.payload`.
        let attempt_details: Vec<serde_json::Value> = decision
            .attempts
            .iter()
            .map(|attempt| {
                serde_json::json!({
                    "parser": attempt.parser,
                    "success": attempt.success,
                    "error": attempt.error,
                    "backend_used": attempt.backend_used,
                    "quality": attempt.quality,
                    "outcome": attempt.outcome,
                    "parsed_sha256": attempt.parsed_sha256,
                    "parsed_bytes": attempt.parsed_bytes,
                })
            })
            .collect();
        Ok(MultiSourceCapture {
            source: self.name().to_string(),
            path_count: 1,
            payload,
            parser: decision.primary_parser.clone(),
            confidence: confidence.to_string(),
            details: serde_json::json!({
                "transport": "ssh_cli",
                "vendor": vendor,
                "command_pattern": command_pattern,
                "ssh_host": ssh_target.host,
                "ssh_port": ssh_target.port,
                "consensus_state": decision.consensus_state,
                "parse_quality": decision.quality,
                "attempts": attempt_details,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct SshTarget {
    host: String,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct CliCaptureResult {
    raw_output: String,
}

pub(crate) fn inferred_vendor(target: &TargetConfig) -> String {
    crate::platform_registry::resolve_target_platform(target)
        .platform_id
        .to_string()
}

pub fn command_for_vendor(vendor: &str) -> Option<&'static str> {
    crate::platform_registry::lookup_platform(vendor).cli_running_config_cmd
}

pub fn ssh_target_host(target: &TargetConfig) -> Result<String> {
    crate::platform_registry::extract_endpoint_host(&target.address)
}

fn ssh_target(target: &TargetConfig) -> Result<SshTarget> {
    let host = ssh_target_host(target)?;
    Ok(SshTarget {
        host,
        port: DEFAULT_SSH_PORT,
    })
}

async fn capture_raw_output(
    target: &SshTarget,
    credentials: &ResolvedCredential,
    command_pattern: &str,
) -> Result<String> {
    let script_path = Path::new(CLI_CAPTURE_SCRIPT);
    if !script_path.exists() {
        bail!("CLI capture helper '{}' is missing", CLI_CAPTURE_SCRIPT);
    }

    let mut failures = Vec::new();
    for candidate in python_candidates() {
        let mut cmd = Command::new(&candidate.program);
        for arg in &candidate.prefix_args {
            cmd.arg(arg);
        }
        cmd.arg(script_path)
            .arg("--host")
            .arg(&target.host)
            .arg("--port")
            .arg(target.port.to_string())
            .arg("--username")
            .arg(&credentials.username)
            .arg("--command")
            .arg(command_pattern)
            .env("BONSAI_CAPTURE_PASSWORD", &credentials.password)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match cmd.output().await {
            Ok(output) if output.status.success() => {
                let result: CliCaptureResult = serde_json::from_slice(&output.stdout)
                    .with_context(|| {
                        format!(
                            "CLI capture helper returned invalid JSON via {}",
                            candidate.display()
                        )
                    })?;
                return Ok(result.raw_output);
            }
            Ok(output) => {
                failures.push(format!(
                    "{} exited with {}: {}",
                    candidate.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Err(error) => {
                failures.push(format!("{} failed to start: {error}", candidate.display()));
            }
        }
    }

    bail!(
        "CLI capture helper could not be executed for {}:{} ({})",
        target.host,
        target.port,
        failures.join("; ")
    )
}

#[derive(Clone, Debug)]
struct PythonCandidate {
    program: OsString,
    prefix_args: Vec<OsString>,
}

impl PythonCandidate {
    fn display(&self) -> String {
        let mut parts = vec![self.program.to_string_lossy().to_string()];
        parts.extend(
            self.prefix_args
                .iter()
                .map(|arg| arg.to_string_lossy().to_string()),
        );
        parts.join(" ")
    }
}

fn python_candidates() -> Vec<PythonCandidate> {
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("BONSAI_PYTHON_BIN")
        && !explicit.trim().is_empty()
    {
        candidates.push(PythonCandidate {
            program: OsString::from(explicit),
            prefix_args: Vec::new(),
        });
    }

    let repo_local = [
        PathBuf::from(".venv/bin/python3"),
        PathBuf::from(".venv/Scripts/python.exe"),
    ];
    for path in repo_local {
        if path.exists() {
            candidates.push(PythonCandidate {
                program: path.into_os_string(),
                prefix_args: Vec::new(),
            });
        }
    }

    candidates.push(PythonCandidate {
        program: OsString::from("python3"),
        prefix_args: Vec::new(),
    });
    candidates.push(PythonCandidate {
        program: OsString::from("python"),
        prefix_args: Vec::new(),
    });
    candidates.push(PythonCandidate {
        program: OsString::from("py"),
        prefix_args: vec![OsString::from("-3")],
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_command_mapping_covers_current_lab_vendors() {
        assert_eq!(
            command_for_vendor("nokia-srlinux"),
            Some("info from running")
        );
        assert_eq!(
            command_for_vendor("cisco-iosxr"),
            Some("show running-config")
        );
        assert_eq!(
            command_for_vendor("juniper-junos"),
            Some("show configuration | display set")
        );
        assert_eq!(
            command_for_vendor("arista-eos"),
            Some("show running-config")
        );
    }

    #[test]
    fn ssh_target_uses_host_portion_of_gnmi_address() {
        let target = TargetConfig {
            address: "172.100.102.21:57400".to_string(),
            enabled: true,
            vendor: Some("cisco-iosxr".to_string()),
            ..Default::default()
        };

        let ssh_target = ssh_target(&target).expect("ssh target");
        assert_eq!(ssh_target.host, "172.100.102.21");
        assert_eq!(ssh_target.port, DEFAULT_SSH_PORT);
    }

    #[test]
    fn vendor_inference_requires_explicit_vendor_while_hostname_is_advisory_only() {
        let target = TargetConfig {
            address: "172.100.102.21:57400".to_string(),
            enabled: true,
            vendor: None,
            hostname: Some("xrd-pe1".to_string()),
            ..Default::default()
        };

        assert_eq!(inferred_vendor(&target), "unknown");
        assert_eq!(
            crate::platform_registry::suggest_platform_from_hostname(target.hostname.as_deref()),
            Some("cisco.iosxr")
        );
    }

    /// CR-02 production-path regression: the vendor string produced by
    /// `inferred_vendor()` (canonical dotted platform id) must reach the
    /// sidecar translated to the backend vendor id (`cisco_iosxr`), exercised
    /// end-to-end through `ParserChain::parse` — not just a mapping helper.
    #[tokio::test]
    async fn inferred_vendor_output_reaches_sidecar_as_backend_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxr",
                "command": "show running-config",
                "parsed_json": {"ok": true},
            })))
            .mount(&server)
            .await;

        let mut priorities = std::collections::HashMap::new();
        priorities.insert(
            "cisco.iosxr::*".to_string(),
            vec!["pyats_genie".to_string()],
        );
        let chain = crate::parser_chain::ParserChain::new(crate::config::ParserChainConfig {
            sidecars: crate::config::ParserSidecarConfig {
                pyats_url: server.uri(),
                native_url: "http://127.0.0.1:1".to_string(),
            },
            priorities,
            ..Default::default()
        });

        let target = TargetConfig {
            address: "172.100.102.21:57400".to_string(),
            enabled: true,
            vendor: Some("cisco-iosxr".to_string()),
            ..Default::default()
        };
        let vendor = inferred_vendor(&target);
        assert_eq!(vendor, "cisco.iosxr");
        let command = command_for_vendor(&vendor).expect("iosxr command");

        let decision = chain
            .parse(ParseRequest {
                vendor,
                command_pattern: command.to_string(),
                raw_output: "x\n".to_string(),
            })
            .await
            .expect("parse decision");
        assert_eq!(decision.quality, crate::parser_chain::QUALITY_STRUCTURED);

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("request json");
        assert_eq!(body["vendor"], "cisco_iosxr");
        assert_eq!(body["parser"], "genie");
        assert_eq!(body["command"], command);
    }
}

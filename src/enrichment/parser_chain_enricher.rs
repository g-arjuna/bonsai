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
        Ok(MultiSourceCapture {
            source: self.name().to_string(),
            path_count: 1,
            payload,
            parser: decision.primary_parser.clone(),
            confidence: "medium".to_string(),
            details: serde_json::json!({
                "transport": "ssh_cli",
                "vendor": vendor,
                "command_pattern": command_pattern,
                "ssh_host": ssh_target.host,
                "ssh_port": ssh_target.port,
                "consensus_state": decision.consensus_state,
                "attempts": decision.attempts,
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
}

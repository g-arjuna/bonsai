use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{ParserChainConfig, ParserSidecarConfig};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseRequest {
    pub vendor: String,
    pub command_pattern: String,
    pub raw_output: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseResponse {
    pub parser: String,
    pub parsed_json: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseAttempt {
    pub parser: String,
    pub success: bool,
    pub error: String,
    pub parsed_json: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseDecision {
    pub primary_parser: String,
    pub consensus_state: String,
    pub parsed_json: serde_json::Value,
    pub attempts: Vec<ParseAttempt>,
}

pub struct ParserChain {
    config: ParserChainConfig,
    client: SidecarParserClient,
}

impl ParserChain {
    pub fn new(config: ParserChainConfig) -> Self {
        let client = SidecarParserClient::new(config.sidecars.clone());
        Self { config, client }
    }

    pub async fn parse(&self, request: ParseRequest) -> Result<ParseDecision> {
        let priorities = self.priorities_for(&request.vendor, &request.command_pattern);
        if priorities.is_empty() {
            bail!(
                "no parser priority chain configured for vendor '{}' command '{}'",
                request.vendor,
                request.command_pattern
            );
        }

        let mut attempts = Vec::new();
        let mut primary: Option<ParseResponse> = None;
        for parser in &priorities {
            match self.client.parse(parser, &request).await {
                Ok(response) => {
                    attempts.push(ParseAttempt {
                        parser: parser.clone(),
                        success: true,
                        error: String::new(),
                        parsed_json: response.parsed_json.clone(),
                    });
                    if primary.is_none() {
                        primary = Some(response);
                        if !self.config.consensus_mode {
                            break;
                        }
                    }
                }
                Err(error) => attempts.push(ParseAttempt {
                    parser: parser.clone(),
                    success: false,
                    error: error.to_string(),
                    parsed_json: serde_json::Value::Null,
                }),
            }
        }

        let primary = primary.ok_or_else(|| {
            anyhow::anyhow!(
                "all parser-chain attempts failed for vendor '{}' command '{}'",
                request.vendor,
                request.command_pattern
            )
        })?;
        let consensus_state = consensus_state(&attempts, &primary.parsed_json);
        Ok(ParseDecision {
            primary_parser: primary.parser,
            consensus_state,
            parsed_json: primary.parsed_json,
            attempts,
        })
    }

    fn priorities_for(&self, vendor: &str, command_pattern: &str) -> Vec<String> {
        let exact_key = format!(
            "{}::{}",
            vendor.trim().to_ascii_lowercase(),
            command_pattern.trim()
        );
        if let Some(priorities) = self.config.priorities.get(&exact_key) {
            return priorities.clone();
        }
        let wildcard_key = format!("{}::*", vendor.trim().to_ascii_lowercase());
        self.config
            .priorities
            .get(&wildcard_key)
            .cloned()
            .unwrap_or_else(default_priorities)
    }
}

fn consensus_state(attempts: &[ParseAttempt], primary: &serde_json::Value) -> String {
    let successful = attempts
        .iter()
        .filter(|attempt| attempt.success)
        .collect::<Vec<_>>();
    if successful.len() <= 1 {
        return "single-parser".to_string();
    }
    if successful
        .iter()
        .all(|attempt| attempt.parsed_json == *primary)
    {
        "agreed".to_string()
    } else {
        "disagreed".to_string()
    }
}

fn default_priorities() -> Vec<String> {
    vec![
        "bonsai_native".to_string(),
        "pyats_genie".to_string(),
        "ntc_templates".to_string(),
    ]
}

struct SidecarParserClient {
    http: reqwest::Client,
    sidecars: ParserSidecarConfig,
}

impl SidecarParserClient {
    fn new(sidecars: ParserSidecarConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            sidecars,
        }
    }

    async fn parse(&self, parser: &str, request: &ParseRequest) -> Result<ParseResponse> {
        match parser {
            "bonsai_native" => Ok(ParseResponse {
                parser: parser.to_string(),
                parsed_json: native_parse(&request.raw_output),
            }),
            "pyats_genie" => {
                self.call_sidecar(&self.sidecars.pyats_url, parser, request)
                    .await
            }
            "ntc_templates" | "suzieq_native" => {
                self.call_sidecar(&self.sidecars.native_url, parser, request)
                    .await
            }
            other => bail!("unsupported parser backend '{other}'"),
        }
    }

    async fn call_sidecar(
        &self,
        base_url: &str,
        parser: &str,
        request: &ParseRequest,
    ) -> Result<ParseResponse> {
        let response = self
            .http
            .post(format!("{}/parse", base_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "parser": parser,
                "vendor": request.vendor,
                "command_pattern": request.command_pattern,
                "raw_output": request.raw_output,
            }))
            .send()
            .await
            .with_context(|| format!("failed to call parser sidecar '{base_url}'"))?;
        if !response.status().is_success() {
            bail!("parser sidecar '{base_url}' returned {}", response.status());
        }
        response
            .json::<ParseResponse>()
            .await
            .context("invalid parser sidecar response")
    }
}

fn native_parse(raw_output: &str) -> serde_json::Value {
    let lines = raw_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    serde_json::json!({
        "line_count": lines.len(),
        "lines": lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> ParserChainConfig {
        let mut priorities = HashMap::new();
        priorities.insert(
            "cisco-iosxr::show bgp neighbors".to_string(),
            vec!["bonsai_native".to_string()],
        );
        ParserChainConfig {
            priorities,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn parser_chain_uses_vendor_command_key() {
        let chain = ParserChain::new(test_config());
        let decision = chain
            .parse(ParseRequest {
                vendor: "cisco-iosxr".to_string(),
                command_pattern: "show bgp neighbors".to_string(),
                raw_output: "neighbor 10.0.0.1\nstate established\n".to_string(),
            })
            .await
            .expect("parse decision");
        assert_eq!(decision.primary_parser, "bonsai_native");
        assert_eq!(decision.consensus_state, "single-parser");
    }

    #[tokio::test]
    async fn parser_chain_calls_sidecar_and_records_consensus() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .and(body_json(serde_json::json!({
                "parser": "pyats_genie",
                "vendor": "cisco-iosxr",
                "command_pattern": "show bgp summary",
                "raw_output": "Neighbor 10.0.0.1 Established"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "parser": "pyats_genie",
                "parsed_json": {"neighbors": 1, "state": "Established"}
            })))
            .mount(&server)
            .await;

        let mut priorities = HashMap::new();
        priorities.insert(
            "cisco-iosxr::show bgp summary".to_string(),
            vec!["pyats_genie".to_string(), "bonsai_native".to_string()],
        );
        let chain = ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url: server.uri(),
                native_url: server.uri(),
            },
            priorities,
            consensus_mode: true,
        });

        let decision = chain
            .parse(ParseRequest {
                vendor: "cisco-iosxr".to_string(),
                command_pattern: "show bgp summary".to_string(),
                raw_output: "Neighbor 10.0.0.1 Established".to_string(),
            })
            .await
            .expect("parse decision");

        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.consensus_state, "disagreed");
        assert_eq!(decision.attempts.len(), 2);
    }
}

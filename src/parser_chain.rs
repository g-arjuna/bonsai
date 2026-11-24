use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{ParserChainConfig, ParserSidecarConfig};

/// Sidecar request timeout. Parser calls are bounded: an unreachable or hung
/// sidecar must fail the attempt rather than stall the enrichment pipeline.
const SIDECAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Quality classes propagated to consumers so unstructured fallback output is
/// never indistinguishable from structured parser facts.
pub const QUALITY_STRUCTURED: &str = "structured";
pub const QUALITY_UNSTRUCTURED_FALLBACK: &str = "unstructured_fallback";
pub const QUALITY_UNKNOWN: &str = "unknown";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseRequest {
    pub vendor: String,
    /// Literal CLI command captured from the device (e.g. "show ip bgp
    /// summary"). Also used as the "<vendor>::<command>" priority lookup key.
    pub command_pattern: String,
    pub raw_output: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseResponse {
    /// Backend identity echoed by the sidecar. Must match the wire parser name
    /// that was requested.
    pub parser: String,
    pub parsed_json: serde_json::Value,
    /// Which backend actually produced `parsed_json` (e.g. "genie", "textfsm",
    /// "fallback", "line_split"). Absent on older sidecars.
    #[serde(default)]
    pub backend_used: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseAttempt {
    pub parser: String,
    pub success: bool,
    pub error: String,
    pub parsed_json: serde_json::Value,
    /// Backend that produced this attempt's output ("" when the attempt failed).
    #[serde(default)]
    pub backend_used: String,
    /// One of QUALITY_STRUCTURED / QUALITY_UNSTRUCTURED_FALLBACK / QUALITY_UNKNOWN.
    #[serde(default)]
    pub quality: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseDecision {
    pub primary_parser: String,
    pub consensus_state: String,
    pub parsed_json: serde_json::Value,
    pub attempts: Vec<ParseAttempt>,
    /// Quality of the primary (winning) attempt.
    #[serde(default)]
    pub quality: String,
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
        let mut primary: Option<(String, ParseResponse, String)> = None;
        for parser in &priorities {
            match self.client.parse(parser, &request).await {
                Ok(response) => {
                    let backend_used = response.backend_used.clone().unwrap_or_default();
                    let quality = quality_for(response.backend_used.as_deref());
                    attempts.push(ParseAttempt {
                        parser: parser.clone(),
                        success: true,
                        error: String::new(),
                        parsed_json: response.parsed_json.clone(),
                        backend_used: backend_used.clone(),
                        quality: quality.to_string(),
                    });
                    if primary.is_none() {
                        primary = Some((parser.clone(), response, quality.to_string()));
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
                    backend_used: String::new(),
                    quality: QUALITY_UNKNOWN.to_string(),
                }),
            }
        }

        let (primary_name, primary, primary_quality) = primary.ok_or_else(|| {
            let details = attempts
                .iter()
                .filter(|attempt| !attempt.success)
                .map(|attempt| format!("{}: {}", attempt.parser, attempt.error))
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::anyhow!(
                "all parser-chain attempts failed for vendor '{}' command '{}' ({})",
                request.vendor,
                request.command_pattern,
                details
            )
        })?;
        let consensus_state = consensus_state(&attempts, &primary.parsed_json);
        Ok(ParseDecision {
            primary_parser: primary_name,
            consensus_state,
            parsed_json: primary.parsed_json,
            attempts,
            quality: primary_quality,
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

/// Structured-first ordering: unstructured line-split output is a last-resort
/// fallback and must never shadow a real parser backend.
fn default_priorities() -> Vec<String> {
    vec![
        "pyats_genie".to_string(),
        "ntc_templates".to_string(),
        "bonsai_native".to_string(),
    ]
}

/// Map a `backend_used` value reported by a sidecar (or produced locally) to a
/// quality class. Unknown/absent values stay explicit rather than being
/// promoted to structured facts.
fn quality_for(backend_used: Option<&str>) -> &'static str {
    match backend_used {
        Some("genie") | Some("textfsm") => QUALITY_STRUCTURED,
        Some("fallback") | Some("line_split") => QUALITY_UNSTRUCTURED_FALLBACK,
        _ => QUALITY_UNKNOWN,
    }
}

struct SidecarParserClient {
    http: reqwest::Client,
    sidecars: ParserSidecarConfig,
}

impl SidecarParserClient {
    fn new(sidecars: ParserSidecarConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(SIDECAR_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            sidecars,
        }
    }

    /// Dispatch an internal parser-chain name to its backend.
    ///
    /// Wire contract (POST {sidecar}/parse):
    ///   {"parser": <wire parser>, "vendor": ..., "command": <literal CLI
    ///    command>, "command_pattern": <same, for older sidecars>,
    ///    "raw_output": ...}
    /// Response must echo `parser` == the wire parser name and may report
    /// `backend_used` describing which engine produced `parsed_json`.
    async fn parse(&self, parser: &str, request: &ParseRequest) -> Result<ParseResponse> {
        match parser {
            "bonsai_native" => Ok(ParseResponse {
                parser: parser.to_string(),
                parsed_json: native_parse(&request.raw_output),
                backend_used: Some("line_split".to_string()),
            }),
            "pyats_genie" => {
                self.call_sidecar(&self.sidecars.pyats_url, "genie", request)
                    .await
            }
            "ntc_templates" => {
                self.call_sidecar(&self.sidecars.pyats_url, "textfsm", request)
                    .await
            }
            "suzieq_native" => {
                bail!("parser backend 'suzieq_native' is not implemented (BN-024)")
            }
            other => bail!("unsupported parser backend '{other}'"),
        }
    }

    async fn call_sidecar(
        &self,
        base_url: &str,
        wire_parser: &str,
        request: &ParseRequest,
    ) -> Result<ParseResponse> {
        let response = self
            .http
            .post(format!("{}/parse", base_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "parser": wire_parser,
                "vendor": request.vendor,
                "command": request.command_pattern,
                "command_pattern": request.command_pattern,
                "raw_output": request.raw_output,
            }))
            .send()
            .await
            .with_context(|| format!("failed to call parser sidecar '{base_url}'"))?;
        if !response.status().is_success() {
            bail!("parser sidecar '{base_url}' returned {}", response.status());
        }
        let parsed: ParseResponse = response
            .json()
            .await
            .context("invalid parser sidecar response")?;
        if parsed.parser != wire_parser {
            bail!(
                "parser sidecar '{base_url}' returned mismatched backend identity '{}' (expected '{wire_parser}')",
                parsed.parser
            );
        }
        Ok(parsed)
    }
}

fn native_parse(raw_output: &str) -> serde_json::Value {
    let lines = raw_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    serde_json::json!({
        "result_kind": "unstructured_fallback",
        "backend_used": "line_split",
        "line_count": lines.len(),
        "lines": lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use wiremock::matchers::{method, path};
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

    fn chain_with(
        priorities: HashMap<String, Vec<String>>,
        pyats_url: String,
        native_url: String,
    ) -> ParserChain {
        ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url,
                native_url,
            },
            priorities,
            consensus_mode: false,
        })
    }

    fn request() -> ParseRequest {
        ParseRequest {
            vendor: "cisco_iosxe".to_string(),
            command_pattern: "show ip bgp summary".to_string(),
            raw_output: "BGP router identifier 10.0.0.1\n".to_string(),
        }
    }

    fn priorities_for(vendor: &str, parsers: &[&str]) -> HashMap<String, Vec<String>> {
        let mut map = HashMap::new();
        map.insert(
            format!("{vendor}::*"),
            parsers.iter().map(|p| p.to_string()).collect(),
        );
        map
    }

    async fn mock_parse(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
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
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(
            decision.parsed_json["result_kind"],
            serde_json::json!("unstructured_fallback")
        );
    }

    #[tokio::test]
    async fn pyats_genie_sends_command_field_and_wire_parser() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"bgp_neighbors": 2},
            }),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert_eq!(decision.attempts[0].backend_used, "genie");

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["parser"], serde_json::json!("genie"));
        assert_eq!(body["command"], serde_json::json!("show ip bgp summary"));
        assert_eq!(body["vendor"], serde_json::json!("cisco_iosxe"));
        assert!(body["raw_output"].is_string());
    }

    #[tokio::test]
    async fn ntc_templates_dispatches_textfsm_backend() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "textfsm",
                "backend_used": "textfsm",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": [{"neighbor": "10.0.0.1"}],
            }),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["ntc_templates"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);

        let requests = server.received_requests().await.expect("requests");
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["parser"], serde_json::json!("textfsm"));
    }

    #[tokio::test]
    async fn suzieq_native_is_explicitly_unsupported() {
        let server = MockServer::start().await;
        mock_parse(&server, serde_json::json!({})).await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["suzieq_native"]),
            server.uri(),
            server.uri(),
        );
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("suzieq_native"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sidecar_fallback_output_is_labeled_unstructured() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "fallback",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"line_count": 1, "lines": ["BGP router identifier 10.0.0.1"]},
            }),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(decision.attempts[0].backend_used, "fallback");
    }

    #[tokio::test]
    async fn missing_backend_used_is_unknown_quality() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"bgp_neighbors": 2},
            }),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.quality, QUALITY_UNKNOWN);
    }

    #[tokio::test]
    async fn mismatched_backend_identity_is_rejected() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "textfsm",
                "backend_used": "textfsm",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": [],
            }),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("backend identity"));
    }

    #[tokio::test]
    async fn default_priorities_try_structured_backends_first() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"bgp_neighbors": 2},
            }),
        )
        .await;
        let chain = chain_with(HashMap::new(), server.uri(), "http://unused".to_string());
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["parser"], serde_json::json!("genie"));
    }

    #[tokio::test]
    async fn sidecar_error_falls_through_to_next_parser() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie", "bonsai_native"]),
            server.uri(),
            "http://unused".to_string(),
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "bonsai_native");
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert!(!decision.attempts[0].success);
        assert!(decision.attempts[0].error.contains("500"));
    }

    #[test]
    fn consensus_state_marks_matching_outputs_as_agreed() {
        let primary = serde_json::json!({"neighbors": 1, "state": "Established"});
        let attempts = vec![
            ParseAttempt {
                parser: "pyats_genie".to_string(),
                success: true,
                error: String::new(),
                parsed_json: primary.clone(),
                backend_used: "genie".to_string(),
                quality: QUALITY_STRUCTURED.to_string(),
            },
            ParseAttempt {
                parser: "bonsai_native".to_string(),
                success: true,
                error: String::new(),
                parsed_json: primary.clone(),
                backend_used: "line_split".to_string(),
                quality: QUALITY_UNSTRUCTURED_FALLBACK.to_string(),
            },
        ];

        assert_eq!(consensus_state(&attempts, &primary), "agreed");
    }

    #[test]
    fn consensus_state_marks_divergent_outputs_as_disagreed() {
        let primary = serde_json::json!({"neighbors": 1, "state": "Established"});
        let attempts = vec![
            ParseAttempt {
                parser: "pyats_genie".to_string(),
                success: true,
                error: String::new(),
                parsed_json: primary.clone(),
                backend_used: "genie".to_string(),
                quality: QUALITY_STRUCTURED.to_string(),
            },
            ParseAttempt {
                parser: "bonsai_native".to_string(),
                success: true,
                error: String::new(),
                parsed_json: serde_json::json!({"line_count": 1}),
                backend_used: "line_split".to_string(),
                quality: QUALITY_UNSTRUCTURED_FALLBACK.to_string(),
            },
        ];

        assert_eq!(consensus_state(&attempts, &primary), "disagreed");
    }
}

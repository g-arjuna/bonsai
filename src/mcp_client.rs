//! Shared MCP (Model Context Protocol) transport client.
//!
//! Enrichers that support both direct REST and MCP transport use this module.
//! Transport is selected via the enricher config's `extra.transport` field:
//!   - `"rest"` (default): enricher calls the target API directly via HTTP
//!   - `"mcp"`:  enricher calls an MCP server, which proxies to the target API
//!
//! The MCP protocol: POST to the server with a JSON-RPC-style body:
//! ```json
//! { "method": "tools/call", "params": { "name": "<tool>", "arguments": { ... } } }
//! ```
//! The server returns `{ "content": [{ "type": "text", "text": "<json>" }] }`.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::time::Duration;

/// Transport mode for enrichers that support both REST and MCP.
#[derive(Debug, Clone, PartialEq)]
pub enum EnricherTransport {
    /// Call the target API (e.g. NetBox REST) directly.
    Rest,
    /// Proxy through an MCP server. `server_url` is the MCP endpoint.
    Mcp { server_url: String },
}

impl EnricherTransport {
    /// Derive from the `extra` JSON field of an `EnricherConfig`.
    /// `extra.transport = "rest"` (default) or `"mcp"`.
    /// `extra.mcp_server_url` is required when `transport = "mcp"`.
    pub fn from_extra(extra: &Value) -> Self {
        let transport = extra
            .get("transport")
            .and_then(|v| v.as_str())
            .unwrap_or("rest");
        if transport == "mcp" {
            let server_url = extra
                .get("mcp_server_url")
                .and_then(|v| v.as_str())
                .unwrap_or("http://localhost:8090")
                .to_string();
            EnricherTransport::Mcp { server_url }
        } else {
            EnricherTransport::Rest
        }
    }
}

/// Shared MCP client. One instance per enricher that chooses MCP transport.
#[derive(Clone)]
pub struct McpClient {
    server_url: String,
    http: reqwest::Client,
}

impl McpClient {
    pub fn new(server_url: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build MCP HTTP client")?;
        Ok(Self { server_url, http })
    }

    /// Call an MCP tool and return the parsed JSON result.
    ///
    /// The tool name should be fully qualified, e.g. `"netbox:devices_list"`.
    /// Arguments are tool-specific key/value pairs.
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        let body = serde_json::json!({
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": arguments,
            }
        });

        let resp = self
            .http
            .post(&self.server_url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("MCP call to {tool} failed"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("MCP server returned {status} for tool {tool}: {text}");
        }

        let json: Value = resp
            .json()
            .await
            .context("failed to parse MCP response as JSON")?;

        // Standard MCP content format: content[0].text is the JSON payload
        let text = json
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .with_context(|| format!("MCP response for {tool} missing content[0].text"))?;

        serde_json::from_str(text)
            .with_context(|| format!("MCP tool {tool} returned non-JSON in content.text"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── EnricherTransport::from_extra ─────────────────────────────────────────

    #[test]
    fn from_extra_empty_defaults_to_rest() {
        assert_eq!(
            EnricherTransport::from_extra(&serde_json::Value::Null),
            EnricherTransport::Rest
        );
    }

    #[test]
    fn from_extra_explicit_rest() {
        let extra = serde_json::json!({"transport": "rest"});
        assert_eq!(EnricherTransport::from_extra(&extra), EnricherTransport::Rest);
    }

    #[test]
    fn from_extra_mcp_uses_provided_server_url() {
        let extra = serde_json::json!({
            "transport": "mcp",
            "mcp_server_url": "http://mcp.example.com:8090"
        });
        assert_eq!(
            EnricherTransport::from_extra(&extra),
            EnricherTransport::Mcp { server_url: "http://mcp.example.com:8090".to_string() }
        );
    }

    #[test]
    fn from_extra_mcp_defaults_server_url_when_missing() {
        let extra = serde_json::json!({"transport": "mcp"});
        match EnricherTransport::from_extra(&extra) {
            EnricherTransport::Mcp { server_url } => {
                assert!(!server_url.is_empty(), "default server_url must not be empty");
            }
            _ => panic!("expected Mcp transport"),
        }
    }

    // ── McpClient::call HTTP behaviour ────────────────────────────────────────

    #[tokio::test]
    async fn call_happy_path_parses_content_text() {
        let server = MockServer::start().await;
        let inner_payload = serde_json::json!({"devices": [{"name": "srl-1"}]});

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&inner_payload).unwrap()}]
                })),
            )
            .mount(&server)
            .await;

        let client = McpClient::new(server.uri()).unwrap();
        let result = client.call("netbox:devices_list", serde_json::json!({})).await.unwrap();
        assert_eq!(result["devices"][0]["name"], "srl-1");
    }

    #[tokio::test]
    async fn call_error_status_surfaces_as_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = McpClient::new(server.uri()).unwrap();
        let err = client.call("netbox:devices_list", serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("500"), "error must mention status code");
    }

    #[tokio::test]
    async fn call_missing_content_text_surfaces_as_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": "ok"})),
            )
            .mount(&server)
            .await;

        let client = McpClient::new(server.uri()).unwrap();
        let err = client.call("netbox:devices_list", serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("content"), "error must mention missing content");
    }
}

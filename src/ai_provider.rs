use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Message / Tool types ──────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AiToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Raw Gemini "parts" array — preserved to replay thought signatures in multi-turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_parts: Option<Vec<serde_json::Value>>,
}

impl AiMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: None, raw_parts: None }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: None, raw_parts: None }
    }
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: Some(tool_call_id.into()),
            raw_parts: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct AiToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct AiResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<AiToolCall>,
    pub stop_reason: String,
    pub tokens_used: u64,
    pub cost_usd: f64,
    /// Raw Gemini "parts" array — includes thought signatures needed for multi-turn.
    pub raw_parts: Option<Vec<serde_json::Value>>,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse>;
    fn name(&self) -> &str;
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Build a provider using the API key supplied directly (vault-backed path).
/// No environment variable lookup is performed.
pub fn build_provider_with_key(cfg: &crate::config::AiConfig, api_key: String) -> Result<Box<dyn AiProvider>> {
    match cfg.provider.as_str() {
        "gemini" => Ok(Box::new(GeminiProvider::new(cfg.model.clone(), api_key))),
        "moonshot" => Ok(Box::new(MoonshotProvider::new(cfg.model.clone(), api_key))),
        "openai" => {
            let base = if cfg.base_url.is_empty() {
                "https://api.openai.com".to_string()
            } else {
                cfg.base_url.trim_end_matches('/').to_string()
            };
            Ok(Box::new(OpenAiProvider::new(cfg.model.clone(), api_key, base)))
        }
        "anthropic" => Ok(Box::new(AnthropicProvider::new(cfg.model.clone(), api_key))),
        "ollama" => {
            let base = if cfg.base_url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                cfg.base_url.trim_end_matches('/').to_string()
            };
            Ok(Box::new(OllamaProvider::new(cfg.model.clone(), base)))
        }
        other => bail!("unsupported AI provider '{}'. Supported: gemini, moonshot, openai, anthropic, ollama", other),
    }
}

/// Build a provider using the env-var path (backward-compat fallback).
/// Prefer build_provider_with_key() when vault credentials are available.
pub fn build_provider(cfg: &crate::config::AiConfig) -> Result<Box<dyn AiProvider>> {
    // Ollama uses no API key — skip the env-var requirement for it
    let api_key = if cfg.provider == "ollama" {
        std::env::var(&cfg.api_key_env).unwrap_or_default()
    } else {
        std::env::var(&cfg.api_key_env)
            .with_context(|| format!("AI API key not set — expected env var '{}'", cfg.api_key_env))?
    };
    build_provider_with_key(cfg, api_key)
}

// ── Gemini ────────────────────────────────────────────────────────────────────

pub struct GeminiProvider {
    model: String,
    api_key: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(model: String, api_key: String) -> Self {
        Self { model, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl AiProvider for GeminiProvider {
    fn name(&self) -> &str { "gemini" }

    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );

        // Convert messages to Gemini "contents" format.
        let mut contents: Vec<serde_json::Value> = Vec::new();
        let mut system_instruction: Option<serde_json::Value> = None;

        for msg in &messages {
            if msg.role == "system" {
                system_instruction = Some(serde_json::json!({
                    "parts": [{"text": msg.content.as_deref().unwrap_or("")}]
                }));
                continue;
            }
            let role = if msg.role == "assistant" { "model" } else { "user" };
            let parts = if let Some(ref rp) = msg.raw_parts {
                // Replay raw parts verbatim — preserves Gemini thought signatures
                rp.clone()
            } else if !msg.tool_calls.is_empty() {
                msg.tool_calls.iter().map(|tc| serde_json::json!({
                    "functionCall": { "name": tc.name, "args": tc.arguments }
                })).collect::<Vec<_>>()
            } else if msg.role == "tool" {
                // Tool result — wrap as functionResponse part in a "user" role content.
                vec![serde_json::json!({
                    "functionResponse": {
                        "name": msg.tool_call_id.as_deref().unwrap_or(""),
                        "response": { "content": msg.content.as_deref().unwrap_or("") }
                    }
                })]
            } else {
                vec![serde_json::json!({"text": msg.content.as_deref().unwrap_or("")})]
            };
            contents.push(serde_json::json!({ "role": role, "parts": parts }));
        }

        let mut body = serde_json::json!({ "contents": contents });

        if let Some(sys) = system_instruction {
            body["system_instruction"] = sys;
        }

        if !tools.is_empty() {
            let fn_decls: Vec<serde_json::Value> = tools.iter().map(|t| serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })).collect();
            body["tools"] = serde_json::json!([{ "function_declarations": fn_decls }]);
        }

        let resp = self.client.post(&url).json(&body).send().await
            .context("Gemini HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await.context("Gemini response read failed")?;
        if !status.is_success() {
            bail!("Gemini API error {}: {}", status, text);
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .context("Gemini response JSON parse failed")?;

        let tokens_used = json["usageMetadata"]["totalTokenCount"]
            .as_u64().unwrap_or(0);
        let cost_usd = gemini_cost_usd(&self.model, tokens_used);

        let candidate = json["candidates"][0]["content"]["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<AiToolCall> = Vec::new();
        for part in candidate.iter() {
            if let Some(text) = part["text"].as_str() {
                text_parts.push(text.to_string());
            }
            if let Some(fc) = part.get("functionCall") {
                let fn_name = fc["name"].as_str().unwrap_or("").to_string();
                tool_calls.push(AiToolCall {
                    id: fn_name.clone(),
                    name: fn_name,
                    arguments: fc["args"].clone(),
                });
            }
        }

        let finish_reason = json["candidates"][0]["finishReason"]
            .as_str().unwrap_or("STOP").to_uppercase();
        let stop_reason = if !tool_calls.is_empty() {
            "tool_use".to_string()
        } else if finish_reason == "STOP" {
            "end_turn".to_string()
        } else {
            finish_reason.to_lowercase()
        };

        Ok(AiResponse {
            content: if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) },
            tool_calls,
            stop_reason,
            tokens_used,
            cost_usd,
            raw_parts: Some(candidate),
        })
    }
}

fn gemini_cost_usd(model: &str, tokens: u64) -> f64 {
    // Approximate blended rate for gemini-2.5-pro
    if model.contains("flash") {
        tokens as f64 * 0.000_000_075
    } else {
        tokens as f64 * 0.000_007_5
    }
}

// ── Moonshot (OpenAI-compatible) ──────────────────────────────────────────────

pub struct MoonshotProvider {
    model: String,
    api_key: String,
    client: reqwest::Client,
}

impl MoonshotProvider {
    pub fn new(model: String, api_key: String) -> Self {
        let model = if model == "gemini-2.5-pro" {
            // Default model name is for Gemini; switch to Moonshot default.
            "moonshot-v1-8k".to_string()
        } else {
            model
        };
        Self { model, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl AiProvider for MoonshotProvider {
    fn name(&self) -> &str { "moonshot" }

    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse> {
        let url = "https://api.moonshot.cn/v1/chat/completions";

        let oai_msgs: Vec<serde_json::Value> = messages.iter().map(|m| {
            let mut obj = serde_json::json!({ "role": m.role });
            obj["content"] = match &m.content {
                Some(c) => serde_json::Value::String(c.clone()),
                None => serde_json::Value::Null,
            };
            if !m.tool_calls.is_empty() {
                obj["tool_calls"] = serde_json::json!(m.tool_calls.iter().map(|tc| serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default()
                    }
                })).collect::<Vec<_>>());
            }
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = serde_json::Value::String(id.clone());
            }
            obj
        }).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": oai_msgs,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })).collect::<Vec<_>>());
        }

        let resp = self.client.post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send().await
            .context("Moonshot HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await.context("Moonshot response read failed")?;
        if !status.is_success() {
            bail!("Moonshot API error {}: {}", status, text);
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .context("Moonshot response JSON parse failed")?;

        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let cost_usd = tokens_used as f64 * 0.000_012;

        let choice = &json["choices"][0];
        let msg = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

        let content = msg["content"].as_str().map(|s| s.to_string());
        let tool_calls: Vec<AiToolCall> = msg["tool_calls"]
            .as_array()
            .map(|tcs| tcs.iter().filter_map(|tc| {
                let fn_obj = tc.get("function")?;
                let args_str = fn_obj["arguments"].as_str().unwrap_or("{}");
                let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or(serde_json::Value::Object(Default::default()));
                Some(AiToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    name: fn_obj["name"].as_str().unwrap_or("").to_string(),
                    arguments: args,
                })
            }).collect())
            .unwrap_or_default();

        let stop_reason = if finish_reason == "tool_calls" {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        };

        Ok(AiResponse { content, tool_calls, stop_reason, tokens_used, cost_usd, raw_parts: None })
    }
}

// ── OpenAI (and Azure OpenAI / any OpenAI-compatible endpoint) ────────────────

pub struct OpenAiProvider {
    model: String,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(model: String, api_key: String, base_url: String) -> Self {
        Self { model, api_key, base_url, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl AiProvider for OpenAiProvider {
    fn name(&self) -> &str { "openai" }

    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let oai_msgs: Vec<serde_json::Value> = messages.iter().map(|m| {
            let mut obj = serde_json::json!({ "role": m.role });
            obj["content"] = match &m.content {
                Some(c) => serde_json::Value::String(c.clone()),
                None => serde_json::Value::Null,
            };
            if !m.tool_calls.is_empty() {
                obj["tool_calls"] = serde_json::json!(m.tool_calls.iter().map(|tc| serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default()
                    }
                })).collect::<Vec<_>>());
            }
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = serde_json::Value::String(id.clone());
            }
            obj
        }).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": oai_msgs,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
            })).collect::<Vec<_>>());
        }

        let resp = self.client.post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send().await
            .context("OpenAI HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await.context("OpenAI response read failed")?;
        if !status.is_success() {
            bail!("OpenAI API error {}: {}", status, text);
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .context("OpenAI response JSON parse failed")?;

        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let cost_usd = openai_cost_usd(&self.model, tokens_used);

        let choice = &json["choices"][0];
        let msg = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

        let content = msg["content"].as_str().map(|s| s.to_string());
        let tool_calls: Vec<AiToolCall> = msg["tool_calls"]
            .as_array()
            .map(|tcs| tcs.iter().filter_map(|tc| {
                let fn_obj = tc.get("function")?;
                let args_str = fn_obj["arguments"].as_str().unwrap_or("{}");
                let args: serde_json::Value = serde_json::from_str(args_str)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                Some(AiToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    name: fn_obj["name"].as_str().unwrap_or("").to_string(),
                    arguments: args,
                })
            }).collect())
            .unwrap_or_default();

        let stop_reason = if finish_reason == "tool_calls" {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        };

        Ok(AiResponse { content, tool_calls, stop_reason, tokens_used, cost_usd, raw_parts: None })
    }
}

fn openai_cost_usd(model: &str, tokens: u64) -> f64 {
    if model.starts_with("gpt-4o-mini") {
        tokens as f64 * 0.000_000_15
    } else if model.starts_with("gpt-4o") || model.starts_with("gpt-4.1") {
        tokens as f64 * 0.000_002_5
    } else if model.starts_with("gpt-4") {
        tokens as f64 * 0.000_030
    } else if model.starts_with("o1") || model.starts_with("o3") {
        tokens as f64 * 0.000_015
    } else {
        tokens as f64 * 0.000_002
    }
}

// ── Anthropic ─────────────────────────────────────────────────────────────────

pub struct AnthropicProvider {
    model: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(model: String, api_key: String) -> Self {
        let model = if model == "gemini-2.5-pro" || model == "moonshot-v1-8k" {
            "claude-opus-4-5".to_string()
        } else {
            model
        };
        Self { model, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn name(&self) -> &str { "anthropic" }

    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse> {
        let url = "https://api.anthropic.com/v1/messages";

        // Anthropic separates the system message from user/assistant turns
        let system_text: Option<String> = messages.iter()
            .find(|m| m.role == "system")
            .and_then(|m| m.content.clone());

        let anthropic_msgs: Vec<serde_json::Value> = messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                if !m.tool_calls.is_empty() {
                    // assistant tool-use turn
                    let content: Vec<serde_json::Value> = m.tool_calls.iter().map(|tc| serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    })).collect();
                    serde_json::json!({ "role": "assistant", "content": content })
                } else if m.role == "tool" {
                    // tool result (user role in Anthropic)
                    serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                            "content": m.content.as_deref().unwrap_or(""),
                        }]
                    })
                } else {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content.as_deref().unwrap_or(""),
                    })
                }
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 8192,
            "messages": anthropic_msgs,
        });
        if let Some(sys) = system_text {
            body["system"] = serde_json::Value::String(sys);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })).collect::<Vec<_>>());
        }

        let resp = self.client.post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send().await
            .context("Anthropic HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await.context("Anthropic response read failed")?;
        if !status.is_success() {
            bail!("Anthropic API error {}: {}", status, text);
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .context("Anthropic response JSON parse failed")?;

        let input_tokens = json["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
        let tokens_used = input_tokens + output_tokens;
        let cost_usd = anthropic_cost_usd(&self.model, input_tokens, output_tokens);

        let stop_reason_raw = json["stop_reason"].as_str().unwrap_or("end_turn");
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<AiToolCall> = Vec::new();

        if let Some(content) = json["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        tool_calls.push(AiToolCall {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            arguments: block["input"].clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        let stop_reason = if !tool_calls.is_empty() || stop_reason_raw == "tool_use" {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        };

        Ok(AiResponse {
            content: if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) },
            tool_calls,
            stop_reason,
            tokens_used,
            cost_usd,
            raw_parts: None,
        })
    }
}

fn anthropic_cost_usd(model: &str, input: u64, output: u64) -> f64 {
    let (in_rate, out_rate) = if model.contains("opus") {
        (0.000_015, 0.000_075)
    } else if model.contains("sonnet") {
        (0.000_003, 0.000_015)
    } else if model.contains("haiku") {
        (0.000_000_25, 0.000_001_25)
    } else {
        (0.000_003, 0.000_015)
    };
    input as f64 * in_rate + output as f64 * out_rate
}

// ── Ollama (local, OpenAI-compatible /api/chat endpoint) ──────────────────────

pub struct OllamaProvider {
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(model: String, base_url: String) -> Self {
        Self { model, base_url, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl AiProvider for OllamaProvider {
    fn name(&self) -> &str { "ollama" }

    async fn complete(&self, messages: Vec<AiMessage>, tools: Vec<AiToolDef>) -> Result<AiResponse> {
        // Use OpenAI-compatible endpoint that newer Ollama versions support
        let url = format!("{}/v1/chat/completions", self.base_url);

        let oai_msgs: Vec<serde_json::Value> = messages.iter().map(|m| {
            let mut obj = serde_json::json!({ "role": m.role });
            obj["content"] = match &m.content {
                Some(c) => serde_json::Value::String(c.clone()),
                None => serde_json::Value::Null,
            };
            if !m.tool_calls.is_empty() {
                obj["tool_calls"] = serde_json::json!(m.tool_calls.iter().map(|tc| serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default()
                    }
                })).collect::<Vec<_>>());
            }
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = serde_json::Value::String(id.clone());
            }
            obj
        }).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": oai_msgs,
            "stream": false,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
            })).collect::<Vec<_>>());
        }

        let resp = self.client.post(&url)
            .json(&body)
            .send().await
            .context("Ollama HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().await.context("Ollama response read failed")?;
        if !status.is_success() {
            bail!("Ollama API error {}: {}", status, text);
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .context("Ollama response JSON parse failed")?;

        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let choice = &json["choices"][0];
        let msg = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

        let content = msg["content"].as_str().map(|s| s.to_string());
        let tool_calls: Vec<AiToolCall> = msg["tool_calls"]
            .as_array()
            .map(|tcs| tcs.iter().filter_map(|tc| {
                let fn_obj = tc.get("function")?;
                let args_str = fn_obj["arguments"].as_str().unwrap_or("{}");
                let args: serde_json::Value = serde_json::from_str(args_str)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                Some(AiToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    name: fn_obj["name"].as_str().unwrap_or("").to_string(),
                    arguments: args,
                })
            }).collect())
            .unwrap_or_default();

        let stop_reason = if finish_reason == "tool_calls" { "tool_use" } else { "end_turn" }.to_string();

        Ok(AiResponse { content, tool_calls, stop_reason, tokens_used, cost_usd: 0.0, raw_parts: None })
    }
}

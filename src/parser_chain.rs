use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{ParserChainConfig, ParserSidecarConfig};

/// Sidecar request timeout. Parser calls are bounded: an unreachable or hung
/// sidecar must fail the attempt rather than stall the enrichment pipeline.
const SIDECAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Total wall-clock budget for one parser-chain invocation across all
/// configured backends. Individual attempts are additionally bounded by
/// `SIDECAR_TIMEOUT`; this cap bounds cumulative chain latency.
const CHAIN_BUDGET: Duration = Duration::from_secs(60);

/// Maximum accepted `raw_output` size. Larger captures are rejected before any
/// backend runs rather than being silently truncated.
const MAX_RAW_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Maximum accepted parser sidecar response body. Oversize responses are
/// rejected as `invalid_response` instead of being buffered without bound.
const MAX_SIDECAR_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Maximum number of backend attempts in one resolved priority chain.
const MAX_CHAIN_BACKENDS: usize = 8;

/// Maximum lines retained by the local unstructured line-split fallback.
const MAX_NATIVE_LINES: usize = 10_000;

/// Quality classes propagated to consumers so unstructured fallback output is
/// never indistinguishable from structured parser facts.
pub const QUALITY_STRUCTURED: &str = "structured";
pub const QUALITY_UNSTRUCTURED_FALLBACK: &str = "unstructured_fallback";
pub const QUALITY_UNKNOWN: &str = "unknown";

/// Per-attempt outcome vocabulary. `quality` describes output class; `outcome`
/// records *why* an attempt ended the way it did, keeping empty results,
/// unsupported commands and failures distinct.
pub const OUTCOME_STRUCTURED_SUCCESS: &str = "structured_success";
pub const OUTCOME_STRUCTURED_EMPTY: &str = "structured_empty";
pub const OUTCOME_UNSTRUCTURED_FALLBACK: &str = "unstructured_fallback";
pub const OUTCOME_UNSUPPORTED: &str = "unsupported";
pub const OUTCOME_BACKEND_UNAVAILABLE: &str = "backend_unavailable";
pub const OUTCOME_TIMEOUT: &str = "timeout";
pub const OUTCOME_INVALID_INPUT: &str = "invalid_input";
pub const OUTCOME_INVALID_RESPONSE: &str = "invalid_response";
pub const OUTCOME_PARSER_ERROR: &str = "parser_error";
pub const OUTCOME_UNKNOWN_QUALITY: &str = "unknown_quality";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseRequest {
    /// Canonical Bonsai platform identity or registry alias (e.g. `cisco.iosxr`,
    /// `cisco_iosxr`). The parser-chain client translates this to the
    /// backend-specific vendor selector at the sidecar boundary.
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
    /// Vendor selector echoed by the sidecar (the translated backend id that
    /// was sent, not necessarily the canonical platform id).
    #[serde(default)]
    pub vendor: Option<String>,
    /// Command echoed by the sidecar.
    #[serde(default)]
    pub command: Option<String>,
    /// Declared parse outcome from newer sidecars
    /// ("structured_success" | "structured_empty" | "unstructured_fallback" |
    /// "invalid_input"). Absent on older sidecars; never fabricated client-side.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Typed per-backend attempt diagnostics reported by newer sidecars:
    /// every backend tried, its declared outcome and a bounded sanitized
    /// reason — preserving *why* structured parsing failed (unsupported,
    /// backend_unavailable, parser_error) separately from the fallback
    /// payload that was ultimately returned. Empty on older sidecars.
    #[serde(default)]
    pub attempts: Vec<SidecarAttempt>,
}

/// One backend attempt inside a sidecar `/parse` response.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidecarAttempt {
    /// Backend identity ("genie", "textfsm", ...).
    pub backend: String,
    /// Declared attempt outcome using the OUTCOME_* vocabulary.
    #[serde(default)]
    pub outcome: String,
    /// Bounded sanitized reason (never raw device output or credentials).
    #[serde(default)]
    pub detail: String,
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
    /// One of the OUTCOME_* constants describing why the attempt ended as it did.
    #[serde(default)]
    pub outcome: String,
    /// Typed sub-attempt diagnostics from the sidecar (why each backend
    /// succeeded or failed before fallback) — empty for the native
    /// line-split backend and older sidecars.
    #[serde(default)]
    pub backend_attempts: Vec<SidecarAttempt>,
    /// SHA-256 of the canonical JSON encoding of `parsed_json` ("" when absent).
    #[serde(default)]
    pub parsed_sha256: String,
    /// Serialized size of `parsed_json` in bytes (0 when absent).
    #[serde(default)]
    pub parsed_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseDecision {
    pub primary_parser: String,
    /// Representation-level comparison state across *structured* attempts only:
    /// "single_structured_result" | "representation_match" |
    /// "representation_difference" | "not_comparable" | "no_structured_result".
    /// This is never semantic consensus — two parsers reading the same capture
    /// share the same underlying evidence.
    pub consensus_state: String,
    pub parsed_json: serde_json::Value,
    pub attempts: Vec<ParseAttempt>,
    /// Quality of the primary (winning) attempt.
    #[serde(default)]
    pub quality: String,
}

/// Internal classification of an executed attempt. An HTTP-200 response is an
/// *executed* attempt, not a successful structured result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Structured parser output (possibly an explicitly empty result).
    Structured { empty: bool },
    /// Unstructured fallback output (line-split style).
    Unstructured,
    /// Response executed but quality cannot be established (legacy sidecar
    /// without `backend_used`). Never promoted to structured authority.
    Unknown,
}

impl Verdict {
    fn quality(self) -> &'static str {
        match self {
            Verdict::Structured { .. } => QUALITY_STRUCTURED,
            Verdict::Unstructured => QUALITY_UNSTRUCTURED_FALLBACK,
            Verdict::Unknown => QUALITY_UNKNOWN,
        }
    }

    fn outcome(self) -> &'static str {
        match self {
            Verdict::Structured { empty: true } => OUTCOME_STRUCTURED_EMPTY,
            Verdict::Structured { empty: false } => OUTCOME_STRUCTURED_SUCCESS,
            Verdict::Unstructured => OUTCOME_UNSTRUCTURED_FALLBACK,
            Verdict::Unknown => OUTCOME_UNKNOWN_QUALITY,
        }
    }

    /// Selection rank: structured evidence always outranks degraded output.
    fn rank(self) -> u8 {
        match self {
            Verdict::Structured { .. } => 0,
            Verdict::Unstructured => 1,
            Verdict::Unknown => 2,
        }
    }
}

/// A classified attempt failure. `outcome` uses the OUTCOME_* vocabulary so
/// consumers can distinguish unsupported commands, unavailable backends,
/// timeouts and contract violations without parsing error strings.
#[derive(Debug)]
struct AttemptFailure {
    outcome: &'static str,
    message: String,
}

impl AttemptFailure {
    fn new(outcome: &'static str, message: impl Into<String>) -> Self {
        Self {
            outcome,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AttemptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AttemptFailure {}

/// A successfully executed and validated backend call.
struct ExecutedAttempt {
    response: ParseResponse,
    verdict: Verdict,
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
        if request.raw_output.len() > MAX_RAW_OUTPUT_BYTES {
            bail!(
                "raw_output for vendor '{}' command '{}' exceeds {} bytes",
                request.vendor,
                request.command_pattern,
                MAX_RAW_OUTPUT_BYTES
            );
        }
        let priorities = self.priorities_for(&request.vendor, &request.command_pattern)?;
        if priorities.is_empty() {
            bail!(
                "no parser priority chain configured for vendor '{}' command '{}'",
                request.vendor,
                request.command_pattern
            );
        }

        let deadline = Instant::now() + CHAIN_BUDGET;
        let mut attempts: Vec<ParseAttempt> = Vec::new();
        // Successfully executed attempts in priority order, with the index of
        // the corresponding `ParseAttempt` for diagnostics.
        let mut executed: Vec<(usize, String, ParseResponse, Verdict)> = Vec::new();
        // Best candidate per verdict rank. Rank 0 (structured) terminates the
        // chain early in non-consensus mode; degraded ranks are retained as
        // fallback while structured backends still get attempted.
        let mut best_by_rank: [Option<usize>; 3] = [None, None, None];

        for parser in &priorities {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                attempts.push(ParseAttempt {
                    parser: parser.clone(),
                    success: false,
                    error: "parser chain budget exhausted".to_string(),
                    parsed_json: serde_json::Value::Null,
                    backend_used: String::new(),
                    quality: QUALITY_UNKNOWN.to_string(),
                    outcome: OUTCOME_TIMEOUT.to_string(),
                    backend_attempts: Vec::new(),
                    parsed_sha256: String::new(),
                    parsed_bytes: 0,
                });
                break;
            };
            let attempt_timeout = remaining.min(SIDECAR_TIMEOUT);
            let result =
                match tokio::time::timeout(attempt_timeout, self.client.parse(parser, &request))
                    .await
                {
                    Ok(result) => result,
                    Err(_) => Err(AttemptFailure::new(
                        OUTCOME_TIMEOUT,
                        format!("parser '{parser}' exceeded the chain time budget"),
                    )),
                };
            match result {
                Ok(executed_attempt) => {
                    let ExecutedAttempt { response, verdict } = executed_attempt;
                    let attempt_index = attempts.len();
                    attempts.push(ParseAttempt {
                        parser: parser.clone(),
                        success: true,
                        error: String::new(),
                        parsed_sha256: payload_digest(&response.parsed_json),
                        parsed_bytes: payload_size(&response.parsed_json),
                        parsed_json: response.parsed_json.clone(),
                        backend_used: response.backend_used.clone().unwrap_or_default(),
                        quality: verdict.quality().to_string(),
                        outcome: verdict.outcome().to_string(),
                        backend_attempts: response.attempts.clone(),
                    });
                    let executed_index = executed.len();
                    executed.push((attempt_index, parser.clone(), response, verdict));
                    let rank = verdict.rank() as usize;
                    if best_by_rank[rank].is_none() {
                        best_by_rank[rank] = Some(executed_index);
                    }
                    // Structured output satisfies the chain: stop here unless
                    // consensus mode requires exercising every backend.
                    if !self.config.consensus_mode && verdict.rank() == 0 {
                        break;
                    }
                }
                Err(failure) => attempts.push(ParseAttempt {
                    parser: parser.clone(),
                    success: false,
                    error: failure.message.clone(),
                    parsed_json: serde_json::Value::Null,
                    backend_used: String::new(),
                    quality: QUALITY_UNKNOWN.to_string(),
                    outcome: failure.outcome.to_string(),
                    backend_attempts: Vec::new(),
                    parsed_sha256: String::new(),
                    parsed_bytes: 0,
                }),
            }
        }

        let consensus_state = consensus_state(&attempts);
        // Selection: best-ranked executed attempt wins; within a rank the
        // earliest (highest-priority) backend is kept. Degraded ranks are only
        // permissible when the configuration allows non-structured output.
        let mut chosen = best_by_rank[0];
        if chosen.is_none() && self.config.allow_unstructured_fallback {
            chosen = best_by_rank[1].or(best_by_rank[2]);
        }
        let Some(executed_index) = chosen else {
            let details = attempts
                .iter()
                .map(|attempt| {
                    if attempt.success {
                        format!("{}: {}", attempt.parser, attempt.outcome)
                    } else {
                        format!("{}: {}", attempt.parser, attempt.error)
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            bail!(
                "no permissible parser result for vendor '{}' command '{}' ({})",
                request.vendor,
                request.command_pattern,
                details
            );
        };
        let (_attempt_index, primary_parser, response, verdict) = &executed[executed_index];
        Ok(ParseDecision {
            primary_parser: primary_parser.clone(),
            consensus_state,
            parsed_json: response.parsed_json.clone(),
            attempts,
            quality: verdict.quality().to_string(),
        })
    }

    fn priorities_for(&self, vendor: &str, command_pattern: &str) -> Result<Vec<String>> {
        let vendor_keys = priority_vendor_keys(vendor);
        let mut priorities: Vec<String> = Vec::new();
        'outer: for key_vendor in &vendor_keys {
            for suffix in [command_pattern.trim(), "*"] {
                let key = format!("{key_vendor}::{suffix}");
                if let Some(configured) = self.config.priorities.get(&key) {
                    for parser in configured {
                        if !priorities.contains(parser) {
                            priorities.push(parser.clone());
                        }
                    }
                    break 'outer;
                }
            }
        }
        if priorities.is_empty() {
            priorities = default_priorities();
        }
        if priorities.len() > MAX_CHAIN_BACKENDS {
            bail!(
                "parser priority chain for vendor '{vendor}' exceeds {MAX_CHAIN_BACKENDS} backends"
            );
        }
        Ok(priorities)
    }
}

/// Candidate vendor keys for priority lookup, most specific first: the raw
/// configured vendor string, the canonical platform id, then the
/// backend-specific parser id. Registry aliases keep legacy underscore/dash
/// config keys working while canonical dotted ids resolve identically.
fn priority_vendor_keys(vendor: &str) -> Vec<String> {
    let mut keys = vec![vendor.trim().to_ascii_lowercase()];
    let profile = crate::platform_registry::lookup_platform(vendor);
    for candidate in [profile.platform_id, profile.parser_backend_id.unwrap_or("")] {
        if !candidate.is_empty() {
            let lowered = candidate.to_ascii_lowercase();
            if !keys.contains(&lowered) {
                keys.push(lowered);
            }
        }
    }
    keys
}

/// Representation-level comparison across structured attempts only. Two
/// parsers reading the same capture share the same underlying evidence, so
/// this never claims semantic corroboration; it only reports whether the
/// structured payloads are byte-identical representations.
fn consensus_state(attempts: &[ParseAttempt]) -> String {
    let structured: Vec<&ParseAttempt> = attempts
        .iter()
        .filter(|a| a.success && a.quality == QUALITY_STRUCTURED)
        .collect();
    match structured.len() {
        0 => {
            if attempts.iter().any(|a| a.success) {
                "not_comparable".to_string()
            } else {
                "no_structured_result".to_string()
            }
        }
        1 => "single_structured_result".to_string(),
        _ => {
            let primary = &structured[0].parsed_json;
            if structured
                .iter()
                .all(|attempt| attempt.parsed_json == *primary)
            {
                "representation_match".to_string()
            } else {
                "representation_difference".to_string()
            }
        }
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

fn payload_digest(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

fn payload_size(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0)
}

struct SidecarParserClient {
    /// `None` when the bounded client failed to construct — sidecar calls then
    /// fail explicitly instead of silently running without a timeout.
    http: Option<reqwest::Client>,
    sidecars: ParserSidecarConfig,
}

impl SidecarParserClient {
    fn new(sidecars: ParserSidecarConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(SIDECAR_TIMEOUT)
                .build()
                .ok(),
            sidecars,
        }
    }

    /// Dispatch an internal parser-chain name to its backend and return the
    /// executed attempt with its validated verdict.
    ///
    /// Wire contract (POST {sidecar}/parse):
    ///   {"parser": <wire parser>, "vendor": <backend vendor id>,
    ///    "command": <literal CLI command>, "command_pattern": <same, for
    ///    older sidecars>, "raw_output": ...}
    /// Response must echo `parser` == the wire parser name, echo `vendor` /
    /// `command` when present, and report a `backend_used` consistent with the
    /// requested parser.
    async fn parse(
        &self,
        parser: &str,
        request: &ParseRequest,
    ) -> std::result::Result<ExecutedAttempt, AttemptFailure> {
        match parser {
            "bonsai_native" => {
                let response = ParseResponse {
                    parser: parser.to_string(),
                    parsed_json: native_parse(&request.raw_output),
                    backend_used: Some("line_split".to_string()),
                    vendor: Some(request.vendor.clone()),
                    command: Some(request.command_pattern.clone()),
                    outcome: Some(OUTCOME_UNSTRUCTURED_FALLBACK.to_string()),
                    attempts: Vec::new(),
                };
                Ok(ExecutedAttempt {
                    verdict: Verdict::Unstructured,
                    response,
                })
            }
            "pyats_genie" => {
                self.call_sidecar(&self.sidecars.pyats_url, "genie", request)
                    .await
            }
            "ntc_templates" => {
                self.call_sidecar(&self.sidecars.pyats_url, "textfsm", request)
                    .await
            }
            "suzieq_native" => Err(AttemptFailure::new(
                OUTCOME_UNSUPPORTED,
                "parser backend 'suzieq_native' is not implemented (BN-024)",
            )),
            other => Err(AttemptFailure::new(
                OUTCOME_UNSUPPORTED,
                format!("unsupported parser backend '{other}'"),
            )),
        }
    }

    async fn call_sidecar(
        &self,
        base_url: &str,
        wire_parser: &str,
        request: &ParseRequest,
    ) -> std::result::Result<ExecutedAttempt, AttemptFailure> {
        let http = self.http.as_ref().ok_or_else(|| {
            AttemptFailure::new(
                OUTCOME_BACKEND_UNAVAILABLE,
                "parser sidecar HTTP client unavailable",
            )
        })?;
        // Explicit translation boundary: canonical Bonsai platform identity →
        // backend-specific vendor selector. Platforms without a verified
        // mapping fail as unsupported rather than masquerading as parseable.
        let sidecar_vendor = crate::platform_registry::lookup_platform(&request.vendor)
            .parser_backend_id
            .ok_or_else(|| {
                AttemptFailure::new(
                    OUTCOME_UNSUPPORTED,
                    format!(
                        "no parser backend vendor mapping for platform '{}'",
                        request.vendor
                    ),
                )
            })?
            .to_string();
        let url = format!("{}/parse", base_url.trim_end_matches('/'));
        let response = http
            .post(&url)
            .json(&serde_json::json!({
                "parser": wire_parser,
                "vendor": sidecar_vendor,
                "command": request.command_pattern,
                "command_pattern": request.command_pattern,
                "raw_output": request.raw_output,
            }))
            .send()
            .await
            .map_err(|error| {
                let outcome = if error.is_timeout() {
                    OUTCOME_TIMEOUT
                } else {
                    OUTCOME_BACKEND_UNAVAILABLE
                };
                AttemptFailure::new(
                    outcome,
                    format!("failed to call parser sidecar '{base_url}': {error}"),
                )
            })?;
        if !response.status().is_success() {
            return Err(AttemptFailure::new(
                OUTCOME_BACKEND_UNAVAILABLE,
                format!("parser sidecar '{base_url}' returned {}", response.status()),
            ));
        }
        if response
            .content_length()
            .is_some_and(|len| len as usize > MAX_SIDECAR_RESPONSE_BYTES)
        {
            return Err(AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!(
                    "parser sidecar '{base_url}' response exceeds {MAX_SIDECAR_RESPONSE_BYTES} bytes"
                ),
            ));
        }
        let body = read_body_bounded(response).await.map_err(|failure| {
            AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!("parser sidecar '{base_url}': {}", failure.message),
            )
        })?;
        let parsed: ParseResponse = serde_json::from_slice(&body).map_err(|error| {
            AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!("invalid parser sidecar response from '{base_url}': {error}"),
            )
        })?;
        classify_response(wire_parser, &sidecar_vendor, request, parsed)
    }
}

/// Read a response body with a hard byte cap. Rejects rather than truncates:
/// a truncated parse result must never be treated as complete evidence.
async fn read_body_bounded(
    mut response: reqwest::Response,
) -> std::result::Result<Vec<u8>, AttemptFailure> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        AttemptFailure::new(
            OUTCOME_BACKEND_UNAVAILABLE,
            format!("failed reading parser sidecar response: {error}"),
        )
    })? {
        if body.len() + chunk.len() > MAX_SIDECAR_RESPONSE_BYTES {
            return Err(AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!("response body exceeds {MAX_SIDECAR_RESPONSE_BYTES} bytes"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Validate a sidecar response against the request that produced it and
/// classify the result. An executed HTTP-200 response is not automatically a
/// successful structured result: backend identity, echoed correlation fields,
/// payload shape and declared outcome must all be consistent.
fn classify_response(
    wire_parser: &str,
    expected_vendor: &str,
    request: &ParseRequest,
    response: ParseResponse,
) -> std::result::Result<ExecutedAttempt, AttemptFailure> {
    if response.parser != wire_parser {
        return Err(AttemptFailure::new(
            OUTCOME_INVALID_RESPONSE,
            format!(
                "parser sidecar returned mismatched backend identity '{}' (expected '{wire_parser}')",
                response.parser
            ),
        ));
    }
    if let Some(vendor) = &response.vendor
        && vendor != expected_vendor
    {
        return Err(AttemptFailure::new(
            OUTCOME_INVALID_RESPONSE,
            format!(
                "parser sidecar echoed vendor '{vendor}' but '{expected_vendor}' was requested"
            ),
        ));
    }
    if let Some(command) = &response.command
        && command != &request.command_pattern
    {
        return Err(AttemptFailure::new(
            OUTCOME_INVALID_RESPONSE,
            format!(
                "parser sidecar echoed command '{command}' but '{}' was requested",
                request.command_pattern
            ),
        ));
    }

    // Payload must be a structured carrier (object or array); scalars and null
    // are never plausible parser results.
    let payload_is_structured_shape =
        response.parsed_json.is_object() || response.parsed_json.is_array();
    if !payload_is_structured_shape {
        return Err(AttemptFailure::new(
            OUTCOME_INVALID_RESPONSE,
            "parser sidecar returned a null/scalar parsed_json payload",
        ));
    }
    let fallback_shaped = is_fallback_shaped(&response.parsed_json);

    let backend = response.backend_used.as_deref().unwrap_or("");
    let verdict = match backend {
        "genie" | "textfsm" => {
            // An explicit parser request must not be satisfied by a different
            // backend's output. "auto" is the only mode permitting either.
            if wire_parser != "auto" && backend != wire_parser {
                return Err(AttemptFailure::new(
                    OUTCOME_INVALID_RESPONSE,
                    format!(
                        "requested parser '{wire_parser}' cannot be satisfied by backend '{backend}'"
                    ),
                ));
            }
            // Structured backends return objects (Genie) or record lists
            // (TextFSM); a fallback-shaped payload mislabeled as structured is
            // a contract violation, not degraded evidence.
            if fallback_shaped {
                return Err(AttemptFailure::new(
                    OUTCOME_INVALID_RESPONSE,
                    format!("backend '{backend}' returned fallback-shaped output"),
                ));
            }
            let empty = response
                .parsed_json
                .as_object()
                .is_some_and(|o| o.is_empty())
                || response
                    .parsed_json
                    .as_array()
                    .is_some_and(|a| a.is_empty());
            Verdict::Structured { empty }
        }
        "fallback" | "line_split" => Verdict::Unstructured,
        "" => Verdict::Unknown,
        other => {
            return Err(AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!("parser sidecar claimed unknown backend '{other}'"),
            ));
        }
    };

    // Sidecar-declared outcome must not contradict the backend/payload
    // classification. Unknown/absent outcome values are ignored rather than
    // promoted — classification above already stands on backend_used + shape.
    if let Some(outcome) = response.outcome.as_deref() {
        if outcome == OUTCOME_INVALID_INPUT {
            return Err(AttemptFailure::new(
                OUTCOME_INVALID_INPUT,
                "parser sidecar reported invalid input",
            ));
        }
        let consistent = match outcome {
            OUTCOME_STRUCTURED_SUCCESS | OUTCOME_STRUCTURED_EMPTY => {
                matches!(verdict, Verdict::Structured { .. })
            }
            OUTCOME_UNSTRUCTURED_FALLBACK => matches!(verdict, Verdict::Unstructured),
            _ => true,
        };
        if !consistent {
            return Err(AttemptFailure::new(
                OUTCOME_INVALID_RESPONSE,
                format!("parser sidecar outcome '{outcome}' contradicts backend classification"),
            ));
        }
    }

    // Sidecar-reported per-backend diagnostics are advisory evidence, but the
    // client still bounds them: an unbounded `attempts` array or detail string
    // must not become a side channel for arbitrary sidecar payloads.
    const MAX_BACKEND_ATTEMPTS: usize = 32;
    const MAX_ATTEMPT_DETAIL_BYTES: usize = 512;
    let mut response = response;
    response.attempts.truncate(MAX_BACKEND_ATTEMPTS);
    for attempt in &mut response.attempts {
        if attempt.detail.len() > MAX_ATTEMPT_DETAIL_BYTES {
            let mut end = MAX_ATTEMPT_DETAIL_BYTES;
            while !attempt.detail.is_char_boundary(end) {
                end -= 1;
            }
            attempt.detail.truncate(end);
        }
        attempt.backend.truncate(64);
    }

    Ok(ExecutedAttempt { response, verdict })
}

/// Detect payloads that are explicitly fallback-shaped (line-split markers)
/// so they cannot be presented as structured parser output.
fn is_fallback_shaped(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("result_kind").and_then(|v| v.as_str()) == Some("unstructured_fallback") {
        return true;
    }
    matches!(
        object.get("backend_used").and_then(|v| v.as_str()),
        Some("line_split") | Some("fallback")
    )
}

fn native_parse(raw_output: &str) -> serde_json::Value {
    let mut lines = Vec::new();
    let mut truncated = false;
    for line in raw_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if lines.len() >= MAX_NATIVE_LINES {
            truncated = true;
            break;
        }
        lines.push(trimmed);
    }
    serde_json::json!({
        "result_kind": "unstructured_fallback",
        "backend_used": "line_split",
        "line_count": lines.len(),
        "truncated": truncated,
        "lines": lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use wiremock::matchers::{body_partial_json, method, path};
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
        consensus_mode: bool,
        allow_fallback: bool,
    ) -> ParserChain {
        ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url,
                native_url: "http://unused".to_string(),
            },
            priorities,
            consensus_mode,
            allow_unstructured_fallback: allow_fallback,
            ..Default::default()
        })
    }

    fn chain_for(priorities: &[&str], server: &MockServer) -> ParserChain {
        chain_with(
            priorities_for("cisco_iosxe", priorities),
            server.uri(),
            false,
            true,
        )
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

    /// Mount a sidecar mock that answers differently per wire `parser` field.
    async fn mock_parse_by_wire_parser(
        server: &MockServer,
        genie_body: serde_json::Value,
        textfsm_body: serde_json::Value,
    ) {
        Mock::given(method("POST"))
            .and(path("/parse"))
            .and(body_partial_json(serde_json::json!({"parser": "genie"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(genie_body))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .and(body_partial_json(serde_json::json!({"parser": "textfsm"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(textfsm_body))
            .mount(server)
            .await;
    }

    fn structured_response(parser: &str, payload: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "parser": parser,
            "backend_used": parser,
            "vendor": "cisco_iosxe",
            "command": "show ip bgp summary",
            "outcome": "structured_success",
            "parsed_json": payload,
        })
    }

    fn fallback_response(parser: &str) -> serde_json::Value {
        serde_json::json!({
            "parser": parser,
            "backend_used": "fallback",
            "vendor": "cisco_iosxe",
            "command": "show ip bgp summary",
            "outcome": "unstructured_fallback",
            "parsed_json": {"line_count": 1, "lines": ["BGP router identifier 10.0.0.1"]},
        })
    }

    // ── CR-01: structured-first must not be shadowed by sidecar fallback ──

    #[tokio::test]
    async fn genie_fallback_does_not_shadow_textfsm() {
        let server = MockServer::start().await;
        mock_parse_by_wire_parser(
            &server,
            fallback_response("genie"),
            structured_response("textfsm", serde_json::json!([{"neighbor": "10.0.0.1"}])),
        )
        .await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "ntc_templates");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert_eq!(
            decision.parsed_json,
            serde_json::json!([{"neighbor": "10.0.0.1"}])
        );
        // Both backends must have been attempted; the fallback stays visible
        // as attempt diagnostics rather than being selected.
        assert_eq!(decision.attempts.len(), 2);
        assert_eq!(decision.attempts[0].parser, "pyats_genie");
        assert_eq!(decision.attempts[0].quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_UNSTRUCTURED_FALLBACK);
        assert!(decision.attempts[0].success);
        assert_eq!(decision.attempts[1].quality, QUALITY_STRUCTURED);
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn genie_unknown_quality_does_not_shadow_textfsm() {
        let server = MockServer::start().await;
        mock_parse_by_wire_parser(
            &server,
            serde_json::json!({
                "parser": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"bgp_neighbors": 2},
            }),
            structured_response("textfsm", serde_json::json!([{"neighbor": "10.0.0.1"}])),
        )
        .await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "ntc_templates");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert_eq!(decision.attempts[0].quality, QUALITY_UNKNOWN);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_UNKNOWN_QUALITY);
    }

    #[tokio::test]
    async fn genie_unsupported_then_textfsm() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .and(body_partial_json(serde_json::json!({"parser": "genie"})))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        mock_parse_by_wire_parser_needs_textfsm(&server).await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "ntc_templates");
        assert_eq!(decision.attempts[0].success, false);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_BACKEND_UNAVAILABLE);
    }

    async fn mock_parse_by_wire_parser_needs_textfsm(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/parse"))
            .and(body_partial_json(serde_json::json!({"parser": "textfsm"})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(structured_response(
                    "textfsm",
                    serde_json::json!([{"neighbor": "10.0.0.1"}]),
                )),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn genie_malformed_structured_claim_rejected_then_textfsm() {
        let server = MockServer::start().await;
        // backend_used claims textfsm under a genie request: contradictory.
        mock_parse_by_wire_parser(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "textfsm",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": [{"neighbor": "10.0.0.1"}],
            }),
            structured_response("textfsm", serde_json::json!([{"neighbor": "10.0.0.1"}])),
        )
        .await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "ntc_templates");
        assert_eq!(decision.attempts[0].success, false);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_INVALID_RESPONSE);
        assert!(decision.attempts[0].error.contains("cannot be satisfied"));
    }

    #[tokio::test]
    async fn fallback_shaped_payload_mislabeled_structured_is_rejected() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {
                    "result_kind": "unstructured_fallback",
                    "backend_used": "line_split",
                    "lines": ["a", "b"],
                },
            }),
        )
        .await;
        let chain = chain_for(&["pyats_genie"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    #[tokio::test]
    async fn null_payload_labeled_structured_is_rejected() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": null,
            }),
        )
        .await;
        let chain = chain_for(&["pyats_genie"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    #[tokio::test]
    async fn unknown_backend_claim_is_rejected() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "vtysh_magic",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "parsed_json": {"x": 1},
            }),
        )
        .await;
        let chain = chain_for(&["pyats_genie"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    #[tokio::test]
    async fn echoed_vendor_and_command_mismatch_rejected() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "arista_eos",
                "command": "show ip bgp summary",
                "parsed_json": {"x": 1},
            }),
        )
        .await;
        let chain = chain_for(&["pyats_genie"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    // ── Fallback policy ──

    #[tokio::test]
    async fn all_structured_unavailable_falls_back_to_native() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let chain = chain_for(&["pyats_genie", "ntc_templates", "bonsai_native"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "bonsai_native");
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(decision.attempts.len(), 3);
        assert!(!decision.attempts[0].success);
        assert!(!decision.attempts[1].success);
        assert!(decision.attempts[2].success);
    }

    #[tokio::test]
    async fn explicit_native_only_configuration_returns_line_split() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let chain = chain_for(&["bonsai_native"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "bonsai_native");
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_UNSTRUCTURED_FALLBACK);
        // No HTTP call should have been made for the local backend.
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fallback_disabled_fails_when_no_structured_result() {
        let server = MockServer::start().await;
        mock_parse(&server, fallback_response("genie")).await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie", "bonsai_native"]),
            server.uri(),
            false,
            false,
        );
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    #[tokio::test]
    async fn structured_empty_is_a_valid_structured_result() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxe",
                "command": "show ip bgp summary",
                "outcome": "structured_empty",
                "parsed_json": {},
            }),
        )
        .await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_STRUCTURED_EMPTY);
        // Structured empty satisfies the chain — textfsm must not run.
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_backend_names_are_deduplicated() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            structured_response("genie", serde_json::json!({"a": 1})),
        )
        .await;
        let chain = chain_for(&["pyats_genie", "pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.attempts.len(), 1);
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
    }

    // ── CR-02: canonical platform id → backend vendor translation ──

    #[tokio::test]
    async fn canonical_platform_id_translates_to_sidecar_vendor() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            serde_json::json!({
                "parser": "genie",
                "backend_used": "genie",
                "vendor": "cisco_iosxr",
                "command": "show ip bgp summary",
                "parsed_json": {"neighbors": 1},
            }),
        )
        .await;
        let mut map = HashMap::new();
        map.insert(
            "cisco.iosxr::*".to_string(),
            vec!["pyats_genie".to_string()],
        );
        let chain = chain_with(map, server.uri(), false, true);
        let decision = chain
            .parse(ParseRequest {
                vendor: "cisco.iosxr".to_string(),
                command_pattern: "show ip bgp summary".to_string(),
                raw_output: "x\n".to_string(),
            })
            .await
            .expect("parse decision");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        let requests = server.received_requests().await.expect("requests");
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        // Canonical "cisco.iosxr" must reach the sidecar as "cisco_iosxr".
        assert_eq!(body["vendor"], serde_json::json!("cisco_iosxr"));
    }

    #[tokio::test]
    async fn unknown_platform_has_no_parser_backend_mapping() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/parse"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let mut map = HashMap::new();
        map.insert("vyos::*".to_string(), vec!["pyats_genie".to_string()]);
        let chain = chain_with(map, server.uri(), false, true);
        let error = chain
            .parse(ParseRequest {
                vendor: "vyos".to_string(),
                command_pattern: "show ip bgp summary".to_string(),
                raw_output: "x\n".to_string(),
            })
            .await
            .expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
        // No HTTP call: unsupported platform fails before reaching the sidecar.
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    // ── Existing contract tests (updated) ──

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
        // Native line-split is unstructured: no structured evidence exists.
        assert_eq!(decision.consensus_state, "not_comparable");
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
            structured_response("genie", serde_json::json!({"bgp_neighbors": 2})),
        )
        .await;
        let chain = chain_for(&["pyats_genie"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert_eq!(decision.attempts[0].backend_used, "genie");
        assert_eq!(decision.attempts[0].outcome, OUTCOME_STRUCTURED_SUCCESS);
        assert!(!decision.attempts[0].parsed_sha256.is_empty());

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
            structured_response("textfsm", serde_json::json!([{"neighbor": "10.0.0.1"}])),
        )
        .await;
        let chain = chain_for(&["ntc_templates"], &server);
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
        let chain = chain_for(&["suzieq_native"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sidecar_fallback_output_is_labeled_unstructured() {
        let server = MockServer::start().await;
        mock_parse(&server, fallback_response("genie")).await;
        let chain = chain_for(&["pyats_genie"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        // Fallback quality is retained and selectable when allowed — but the
        // chain had no structured backend configured, so it is primary.
        assert_eq!(decision.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(decision.attempts[0].backend_used, "fallback");
        assert_eq!(decision.attempts[0].outcome, OUTCOME_UNSTRUCTURED_FALLBACK);
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
        let chain = chain_for(&["pyats_genie"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.quality, QUALITY_UNKNOWN);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_UNKNOWN_QUALITY);
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
        let chain = chain_for(&["pyats_genie"], &server);
        let error = chain.parse(request()).await.expect_err("must fail");
        assert!(error.to_string().contains("no permissible parser result"));
    }

    #[tokio::test]
    async fn default_priorities_try_structured_backends_first() {
        let server = MockServer::start().await;
        mock_parse(
            &server,
            structured_response("genie", serde_json::json!({"bgp_neighbors": 2})),
        )
        .await;
        let chain = chain_with(HashMap::new(), server.uri(), false, true);
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
            .and(body_partial_json(serde_json::json!({"parser": "genie"})))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        mock_parse_by_wire_parser_needs_textfsm(&server).await;
        let chain = chain_for(&["pyats_genie", "ntc_templates"], &server);
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "ntc_templates");
        assert_eq!(decision.quality, QUALITY_STRUCTURED);
        assert!(!decision.attempts[0].success);
        assert_eq!(decision.attempts[0].outcome, OUTCOME_BACKEND_UNAVAILABLE);
        assert!(decision.attempts[0].error.contains("500"));
    }

    #[tokio::test]
    async fn consensus_mode_runs_all_backends() {
        let server = MockServer::start().await;
        mock_parse_by_wire_parser(
            &server,
            structured_response("genie", serde_json::json!({"neighbors": 1})),
            structured_response("textfsm", serde_json::json!([{"neighbor": "10.0.0.1"}])),
        )
        .await;
        let chain = chain_with(
            priorities_for("cisco_iosxe", &["pyats_genie", "ntc_templates"]),
            server.uri(),
            true,
            true,
        );
        let decision = chain.parse(request()).await.expect("parse decision");
        assert_eq!(decision.primary_parser, "pyats_genie");
        assert_eq!(decision.consensus_state, "representation_difference");
        assert_eq!(decision.attempts.len(), 2);
    }

    // ── consensus_state unit tests ──

    fn attempt(
        parser: &str,
        success: bool,
        quality: &str,
        payload: serde_json::Value,
    ) -> ParseAttempt {
        ParseAttempt {
            parser: parser.to_string(),
            success,
            error: String::new(),
            parsed_json: payload,
            backend_used: String::new(),
            quality: quality.to_string(),
            outcome: String::new(),
            backend_attempts: Vec::new(),
            parsed_sha256: String::new(),
            parsed_bytes: 0,
        }
    }

    #[test]
    fn consensus_single_structured_result() {
        let attempts = vec![
            attempt(
                "pyats_genie",
                true,
                QUALITY_STRUCTURED,
                serde_json::json!({"a": 1}),
            ),
            attempt(
                "bonsai_native",
                true,
                QUALITY_UNSTRUCTURED_FALLBACK,
                serde_json::json!({"lines": []}),
            ),
        ];
        assert_eq!(consensus_state(&attempts), "single_structured_result");
    }

    #[test]
    fn consensus_representation_match() {
        let payload = serde_json::json!({"a": 1});
        let attempts = vec![
            attempt("pyats_genie", true, QUALITY_STRUCTURED, payload.clone()),
            attempt("ntc_templates", true, QUALITY_STRUCTURED, payload),
        ];
        assert_eq!(consensus_state(&attempts), "representation_match");
    }

    #[test]
    fn consensus_representation_difference() {
        let attempts = vec![
            attempt(
                "pyats_genie",
                true,
                QUALITY_STRUCTURED,
                serde_json::json!({"a": 1}),
            ),
            attempt(
                "ntc_templates",
                true,
                QUALITY_STRUCTURED,
                serde_json::json!([{"a": 1}]),
            ),
        ];
        assert_eq!(consensus_state(&attempts), "representation_difference");
    }

    #[test]
    fn consensus_not_comparable_without_structured() {
        let attempts = vec![
            attempt(
                "pyats_genie",
                true,
                QUALITY_UNSTRUCTURED_FALLBACK,
                serde_json::json!({"lines": []}),
            ),
            attempt(
                "bonsai_native",
                true,
                QUALITY_UNSTRUCTURED_FALLBACK,
                serde_json::json!({"lines": []}),
            ),
        ];
        assert_eq!(consensus_state(&attempts), "not_comparable");
    }

    #[test]
    fn consensus_no_structured_result_when_all_failed() {
        let attempts = vec![attempt(
            "pyats_genie",
            false,
            QUALITY_UNKNOWN,
            serde_json::Value::Null,
        )];
        assert_eq!(consensus_state(&attempts), "no_structured_result");
    }

    #[test]
    fn native_parse_truncates_at_line_cap() {
        let raw = (0..(MAX_NATIVE_LINES + 5))
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = native_parse(&raw);
        assert_eq!(parsed["line_count"], serde_json::json!(MAX_NATIVE_LINES));
        assert_eq!(parsed["truncated"], serde_json::json!(true));
    }
}

#[cfg(test)]
mod live_sidecar_tests {
    use super::*;
    use std::collections::HashMap;

    /// Genuine packaged-sidecar validation. Requires a running pyats sidecar:
    ///   BONSAI_LIVE_PYATS_URL=http://127.0.0.1:19101 \
    ///   cargo test -p bonsai --lib --profile ci -- --ignored live_pyats
    #[tokio::test]
    #[ignore = "requires a live pyats sidecar (BONSAI_LIVE_PYATS_URL)"]
    async fn live_pyats_sidecar_genie_textfsm_and_fallback() {
        let base = std::env::var("BONSAI_LIVE_PYATS_URL")
            .expect("BONSAI_LIVE_PYATS_URL must point at a running pyats sidecar");
        let raw = "BGP router identifier 10.0.0.1, local AS number 65001\n\
                   BGP table version is 5, main routing table version 5\n\
                   \n\
                   Neighbor        V           AS MsgRcvd MsgSent   TblVer  InQ OutQ Up/Down  State/PfxRcd\n\
                   10.0.0.2        4        65002      10      10        5    0    0 00:05:00        100\n\
                   10.0.0.3        4        65003       8       8        5    0    0 01:00:00        200\n";
        let request = ParseRequest {
            vendor: "cisco_iosxe".to_string(),
            command_pattern: "show ip bgp summary".to_string(),
            raw_output: raw.to_string(),
        };
        let mut priorities = HashMap::new();
        priorities.insert(
            "cisco_iosxe::*".to_string(),
            vec!["pyats_genie".to_string()],
        );
        let chain = ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url: base.clone(),
                native_url: "http://127.0.0.1:1".to_string(),
            },
            priorities: priorities.clone(),
            ..Default::default()
        });
        let genie = chain.parse(request.clone()).await.expect("genie parse");
        assert_eq!(
            genie.quality, QUALITY_STRUCTURED,
            "attempts: {:?}",
            genie.attempts
        );
        assert_eq!(genie.attempts[0].backend_used, "genie");
        assert_eq!(genie.attempts[0].outcome, OUTCOME_STRUCTURED_SUCCESS);

        priorities.insert(
            "cisco_iosxe::*".to_string(),
            vec!["ntc_templates".to_string()],
        );
        let chain = ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url: base.clone(),
                native_url: "http://127.0.0.1:1".to_string(),
            },
            priorities: priorities.clone(),
            ..Default::default()
        });
        let textfsm = chain.parse(request.clone()).await.expect("textfsm parse");
        assert_eq!(
            textfsm.quality, QUALITY_STRUCTURED,
            "attempts: {:?}",
            textfsm.attempts
        );
        assert_eq!(textfsm.attempts[0].backend_used, "textfsm");
        assert!(textfsm.parsed_json.is_array());

        // Unsupported command must degrade to unstructured fallback, never
        // structured success.
        priorities.insert(
            "cisco_iosxe::*".to_string(),
            vec!["pyats_genie".to_string()],
        );
        let chain = ParserChain::new(ParserChainConfig {
            sidecars: ParserSidecarConfig {
                pyats_url: base,
                native_url: "http://127.0.0.1:1".to_string(),
            },
            priorities,
            ..Default::default()
        });
        let bogus = chain
            .parse(ParseRequest {
                vendor: "cisco_iosxe".to_string(),
                command_pattern: "show totally bogus xyzzy".to_string(),
                raw_output: raw.to_string(),
            })
            .await
            .expect("fallback parse");
        assert_eq!(bogus.quality, QUALITY_UNSTRUCTURED_FALLBACK);
        assert_eq!(bogus.attempts[0].backend_used, "fallback");
        assert_eq!(bogus.attempts[0].outcome, OUTCOME_UNSTRUCTURED_FALLBACK);
    }
}

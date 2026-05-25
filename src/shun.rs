/// Syslog Shunning / Suppression Engine (D4-2)
///
/// Evaluates active `ShunRule`s on every inbound syslog event. Two actions:
/// - `drop`: silently discard (event still archived before this check).
/// - `rate_limit`: per-rule token-bucket allowing N events/min; excess dropped.
///
/// Rules are stored in the graph DB (`ShunRule` node table) and cached in
/// memory behind an `RwLock`. Call `reload()` after DB writes to refresh.
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

// ── Data model ────────────────────────────────────────────────────────────────

/// Scope at which a shun rule is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShunScope {
    Device,
    Global,
}

impl ShunScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Global => "global",
        }
    }

    #[allow(dead_code)]
    fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "device" => Self::Device,
            _ => Self::Global,
        }
    }
}

/// How the rule matches a syslog event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShunMatchType {
    Substring,
    Regex,
    FactType,
}

impl ShunMatchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Substring => "substring",
            Self::Regex => "regex",
            Self::FactType => "fact_type",
        }
    }

    #[allow(dead_code)]
    fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "regex" => Self::Regex,
            "fact_type" => Self::FactType,
            _ => Self::Substring,
        }
    }
}

/// What happens when a rule matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShunAction {
    Drop,
    RateLimit,
}

impl ShunAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::RateLimit => "rate_limit",
        }
    }

    #[allow(dead_code)]
    fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "rate_limit" => Self::RateLimit,
            _ => Self::Drop,
        }
    }
}

/// A single shun rule as stored in the DB and served by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShunRule {
    pub id: String,
    /// `device` or `global`.
    pub scope_type: String,
    /// For `device` scope: the device IP/address. Empty for global.
    pub scope_value: String,
    /// `substring`, `regex`, or `fact_type`.
    pub match_type: String,
    /// The pattern to match against the syslog message (or fact_type name).
    pub match_value: String,
    /// `drop` or `rate_limit`.
    pub action: String,
    /// For `rate_limit` action: max events allowed per minute.
    pub rate_limit_per_min: i64,
    /// Unix timestamp (ns). 0 = never expires.
    pub expires_at_ns: i64,
    pub created_by: String,
    pub created_at_ns: i64,
    pub enabled: bool,
}

impl ShunRule {
    pub fn new_drop(scope_type: &str, scope_value: &str, match_type: &str, match_value: &str, expires_at_ns: i64, created_by: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            scope_type: scope_type.to_string(),
            scope_value: scope_value.to_string(),
            match_type: match_type.to_string(),
            match_value: match_value.to_string(),
            action: "drop".to_string(),
            rate_limit_per_min: 0,
            expires_at_ns,
            created_by: created_by.to_string(),
            created_at_ns: now_ns(),
            enabled: true,
        }
    }
}

// ── In-memory evaluation engine ───────────────────────────────────────────────

struct CompiledRule {
    rule: ShunRule,
    regex: Option<Regex>,
}

/// Per-rule token bucket for rate-limiting. Replenishes every 60s.
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(per_min: i64) -> Self {
        let cap = per_min.max(1) as f64;
        Self {
            tokens: cap,
            capacity: cap,
            last_refill: Instant::now(),
        }
    }

    /// Returns true if a token is available (event should pass), false if it should be dropped.
    fn try_consume(&mut self) -> bool {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        if elapsed >= 60.0 {
            self.tokens = self.capacity;
            self.last_refill = Instant::now();
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Outcome of evaluating a syslog event against the active shun rules.
#[derive(Debug, PartialEq, Eq)]
pub enum ShunOutcome {
    /// Event passes all rules — publish normally.
    Pass,
    /// A `drop` rule matched — discard event (already archived).
    Dropped { rule_id: String },
    /// A `rate_limit` rule matched and the token bucket is empty — discard.
    RateLimited { rule_id: String },
}

pub struct ShunEngine {
    rules: RwLock<Vec<CompiledRule>>,
    /// Per-rule token buckets, keyed by rule id.
    buckets: RwLock<HashMap<String, TokenBucket>>,
    /// Stats: total events shunned per rule id.
    shunned_counts: RwLock<HashMap<String, u64>>,
}

impl ShunEngine {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            rules: RwLock::new(Vec::new()),
            buckets: RwLock::new(HashMap::new()),
            shunned_counts: RwLock::new(HashMap::new()),
        })
    }

    /// Replace the active rule set with a freshly loaded list.
    pub fn reload(&self, rules: Vec<ShunRule>) {
        let now_ts = now_ns();
        let compiled: Vec<CompiledRule> = rules
            .into_iter()
            .filter(|r| r.enabled)
            .filter(|r| r.expires_at_ns == 0 || r.expires_at_ns > now_ts)
            .map(|rule| {
                let regex = if rule.match_type == "regex" {
                    match Regex::new(&rule.match_value) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            warn!(rule_id = %rule.id, error = %e, "invalid shun rule regex — rule disabled");
                            None
                        }
                    }
                } else {
                    None
                };
                CompiledRule { rule, regex }
            })
            .filter(|cr| cr.rule.match_type != "regex" || cr.regex.is_some())
            .collect();
        info!(count = compiled.len(), "shun rules reloaded");
        *self.rules.write().expect("shun rules write lock") = compiled;
    }

    /// Evaluate an event. `device_address` is the resolved target address;
    /// `message` is the syslog message body; `fact_types` are any fact_type strings
    /// extracted from this message.
    pub fn evaluate(
        &self,
        device_address: &str,
        message: &str,
        fact_types: &[&str],
    ) -> ShunOutcome {
        let rules = self.rules.read().expect("shun rules read lock");
        for cr in rules.iter() {
            let r = &cr.rule;

            // Scope filter.
            if r.scope_type == "device"
                && !r.scope_value.is_empty()
                && r.scope_value != device_address
            {
                continue;
            }

            // Match check.
            let matched = match r.match_type.as_str() {
                "substring" => message.contains(&r.match_value),
                "regex" => cr.regex.as_ref().is_some_and(|re| re.is_match(message)),
                "fact_type" => fact_types.iter().any(|ft| *ft == r.match_value),
                _ => false,
            };

            if !matched {
                continue;
            }

            // Rule matched — apply action.
            match ShunAction::from_str(&r.action) {
                ShunAction::Drop => {
                    self.record_shun(&r.id);
                    return ShunOutcome::Dropped { rule_id: r.id.clone() };
                }
                ShunAction::RateLimit => {
                    let allow = {
                        let mut buckets = self.buckets.write().expect("shun buckets write lock");
                        let bucket = buckets
                            .entry(r.id.clone())
                            .or_insert_with(|| TokenBucket::new(r.rate_limit_per_min));
                        bucket.try_consume()
                    };
                    if !allow {
                        self.record_shun(&r.id);
                        return ShunOutcome::RateLimited { rule_id: r.id.clone() };
                    }
                    // Token available — pass.
                    return ShunOutcome::Pass;
                }
            }
        }
        ShunOutcome::Pass
    }

    /// Return total shunned count per rule id.
    pub fn stats(&self) -> HashMap<String, u64> {
        self.shunned_counts
            .read()
            .expect("shun stats read lock")
            .clone()
    }

    fn record_shun(&self, rule_id: &str) {
        let mut counts = self.shunned_counts.write().expect("shun stats write lock");
        *counts.entry(rule_id.to_string()).or_default() += 1;
    }
}

impl Default for ShunEngine {
    fn default() -> Self {
        Self {
            rules: RwLock::new(Vec::new()),
            buckets: RwLock::new(HashMap::new()),
            shunned_counts: RwLock::new(HashMap::new()),
        }
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(match_type: &str, match_value: &str, action: &str, rate: i64) -> ShunRule {
        ShunRule {
            id: Uuid::new_v4().to_string(),
            scope_type: "global".to_string(),
            scope_value: String::new(),
            match_type: match_type.to_string(),
            match_value: match_value.to_string(),
            action: action.to_string(),
            rate_limit_per_min: rate,
            expires_at_ns: 0,
            created_by: "test".to_string(),
            created_at_ns: 0,
            enabled: true,
        }
    }

    #[test]
    fn drop_rule_blocks_matching_message() {
        let engine = ShunEngine::new();
        engine.reload(vec![rule("substring", "LICC: License", "drop", 0)]);
        let outcome = engine.evaluate("10.0.0.1", "LICC: License warning", &[]);
        assert!(matches!(outcome, ShunOutcome::Dropped { .. }));
    }

    #[test]
    fn drop_rule_passes_non_matching() {
        let engine = ShunEngine::new();
        engine.reload(vec![rule("substring", "LICC: License", "drop", 0)]);
        let outcome = engine.evaluate("10.0.0.1", "BGP session established", &[]);
        assert_eq!(outcome, ShunOutcome::Pass);
    }

    #[test]
    fn regex_rule_matches() {
        let engine = ShunEngine::new();
        engine.reload(vec![rule("regex", r"%SYS-5-CONFIG_I.*console", "drop", 0)]);
        let outcome = engine.evaluate("10.0.0.1", "%SYS-5-CONFIG_I: Configured from console", &[]);
        assert!(matches!(outcome, ShunOutcome::Dropped { .. }));
    }

    #[test]
    fn rate_limit_allows_first_then_blocks() {
        let engine = ShunEngine::new();
        engine.reload(vec![rule("substring", "noise", "rate_limit", 1)]);
        let first = engine.evaluate("10.0.0.1", "noise message", &[]);
        let second = engine.evaluate("10.0.0.1", "noise message", &[]);
        assert_eq!(first, ShunOutcome::Pass);
        assert!(matches!(second, ShunOutcome::RateLimited { .. }));
    }

    #[test]
    fn device_scope_only_matches_target_device() {
        let engine = ShunEngine::new();
        let mut r = rule("substring", "noise", "drop", 0);
        r.scope_type = "device".to_string();
        r.scope_value = "10.0.0.1".to_string();
        engine.reload(vec![r]);
        assert!(matches!(engine.evaluate("10.0.0.1", "noise", &[]), ShunOutcome::Dropped { .. }));
        assert_eq!(engine.evaluate("10.0.0.2", "noise", &[]), ShunOutcome::Pass);
    }

    #[test]
    fn fact_type_rule_matches() {
        let engine = ShunEngine::new();
        engine.reload(vec![rule("fact_type", "license_warning", "drop", 0)]);
        let outcome = engine.evaluate("10.0.0.1", "anything", &["license_warning"]);
        assert!(matches!(outcome, ShunOutcome::Dropped { .. }));
    }

    #[test]
    fn disabled_rule_is_skipped() {
        let engine = ShunEngine::new();
        let mut r = rule("substring", "noise", "drop", 0);
        r.enabled = false;
        engine.reload(vec![r]);
        assert_eq!(engine.evaluate("10.0.0.1", "noise message", &[]), ShunOutcome::Pass);
    }
}

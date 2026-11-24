//! BN-024-02 — canonical typed-fact ABI.
//!
//! Executable contract for typed facts produced by parser/enrichment
//! pipelines *before* any authoritative graph projection. This module is
//! deliberately write-free: it defines the serialized schema, validation,
//! lifecycle and trust semantics, but contains no graph writer and no path
//! to `graph::` mutation. BN-024-03 may build an extension seam on top once
//! this schema stabilises.
//!
//! Design contract (see docs/bn024/typed-facts/README.md):
//! - `schema_version` is a single integer; deserializers reject versions
//!   newer than `SCHEMA_VERSION` and callers must run explicit upgrades.
//! - Unknown, absent and invalid are first-class states — never collapsed
//!   into fabricated success.
//! - `trust` records *authority eligibility*, separate from parse quality:
//!   structured parser output does not authenticate a device or prove
//!   freshness.
//! - Lifecycle (`active`/`withdrawn`/`superseded`/`stale`) is explicit;
//!   withdrawal requires a reason and a superseding/terminating timestamp.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Serialized schema version. Bump only with a documented evolution rule:
/// additive optional fields keep the same major version; breaking changes
/// require a new version plus an explicit upgrade path.
pub const SCHEMA_VERSION: u32 = 1;

/// Maximum accepted `schema_version` on deserialize. Anything newer is
/// rejected rather than silently mis-parsed.
pub const MAX_SUPPORTED_SCHEMA_VERSION: u32 = SCHEMA_VERSION;

// ── Identity ────────────────────────────────────────────────────────────────

/// Where the observation came from. Field names intentionally mirror the
/// existing `ObservationSource`/`ObservationSourceRef` contracts
/// (collector_id, session_epoch, sequence) so downstream consumers can join
/// BMP and CLI evidence on the same identity axes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactSource {
    /// Bonsai collector/enricher instance that produced the capture.
    pub collector_id: String,
    /// Transport/provenance kind: "cli_ssh", "gnmi", "bmp", "sidecar", ...
    pub source_kind: String,
    /// Capture/session identifier for this observation batch.
    pub capture_id: String,
    /// Monotonic session epoch of the source (BMP session epoch semantics
    /// where applicable; 0 when not applicable).
    pub session_epoch: u64,
    /// Per-source sequence number within the session epoch.
    pub sequence: u64,
}

/// Subject the fact is about. `device_id` is the canonical inventory device
/// identity (never a raw IP); `namespace` scopes the fact kind (e.g.
/// "routing", "interface"); `vrf` is optional instance scoping.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactSubject {
    pub device_id: String,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
}

// ── Payload & quality ───────────────────────────────────────────────────────

/// Typed payload wrapper. `fields` is the structured fact body; absent or
/// invalid content is represented explicitly, never as an empty success.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FactPayload {
    /// Parser/backend that produced the payload (e.g. "genie", "textfsm",
    /// "line_split") — provenance, not authority.
    pub produced_by: String,
    /// Raw structured fields. `None` means "no typed fields extracted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Value>,
    /// Explicit state when `fields` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PayloadState>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadState {
    /// Parser ran successfully but produced no typed fields.
    Empty,
    /// Input was rejected before parsing (empty/oversized/malformed).
    InvalidInput,
    /// No configured backend could produce output.
    Unsupported,
    /// Backend produced unstructured (non-typed) output.
    UnstructuredFallback,
    /// Backend output quality could not be established.
    UnknownQuality,
}

/// Snapshot completeness of the observation the fact was drawn from.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    /// The capture is known to be complete for its declared scope.
    Complete,
    /// Known gaps exist; `missing_fields`/notes describe them.
    Partial,
    /// Completeness cannot be established.
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Quality {
    pub completeness: Completeness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_fields: Vec<String>,
    /// Free-form caveats (truncation, partial command output, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

// ── Trust & authority ───────────────────────────────────────────────────────

/// Trust class — authority *eligibility*, deliberately separate from parse
/// quality. A structured parse from an unauthenticated capture is still only
/// `DeviceAsserted`; nothing here writes to the graph.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// No trust basis established.
    Unverified = 0,
    /// Device/parser asserted the value; transport/authenticity unproven.
    DeviceAsserted = 1,
    /// Corroborated by a second independent observation path.
    Corroborated = 2,
    /// Eligible for authoritative projection (requires corroboration or a
    /// trusted direct channel — enforced by consumers, not this module).
    Authoritative = 3,
}

impl TrustClass {
    /// Whether this class may feed an authoritative projection. Only
    /// `Corroborated` and `Authoritative` qualify; `DeviceAsserted` remains
    /// advisory. This is the trust/authority boundary of the ABI.
    pub fn authority_eligible(self) -> bool {
        matches!(self, TrustClass::Corroborated | TrustClass::Authoritative)
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    /// Explicitly retracted; requires `withdrawn_reason`.
    Withdrawn,
    /// Replaced by a newer fact (`superseded_by` set).
    Superseded,
    /// Known to be older than its validity window.
    Stale,
}

// ── Evidence ────────────────────────────────────────────────────────────────

/// Reference to evidence backing the fact: raw capture digest, sidecar
/// response digest, fixture, etc.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    /// Evidence kind: "raw_output", "sidecar_response", "fixture", ...
    pub kind: String,
    /// SHA-256 hex digest of the evidence content.
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

// ── The fact ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypedFact {
    pub schema_version: u32,
    /// Stable identity: sha256 over canonical (subject, fact_kind,
    /// payload_sha256, source.capture_id) — assigned by `seal()`.
    pub fact_id: String,
    /// Dotted fact kind, e.g. "bgp.neighbor.summary", "interface.state".
    pub fact_kind: String,
    pub subject: FactSubject,
    pub source: FactSource,
    /// RFC-3339 observation timestamp.
    pub observed_at: String,
    /// Validity window (RFC-3339); `None` = validity unknown, not infinite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    pub payload: FactPayload,
    pub quality: Quality,
    pub trust: TrustClass,
    pub lifecycle: Lifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactError {
    UnsupportedSchemaVersion(u32),
    MissingField(&'static str),
    InvalidField(&'static str),
    LifecycleViolation(&'static str),
    TrustViolation(&'static str),
}

impl std::fmt::Display for FactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactError::UnsupportedSchemaVersion(v) => {
                write!(
                    f,
                    "unsupported schema_version {v} (max {MAX_SUPPORTED_SCHEMA_VERSION})"
                )
            }
            FactError::MissingField(k) => write!(f, "missing required field '{k}'"),
            FactError::InvalidField(k) => write!(f, "invalid field '{k}'"),
            FactError::LifecycleViolation(k) => write!(f, "lifecycle violation: {k}"),
            FactError::TrustViolation(k) => write!(f, "trust violation: {k}"),
        }
    }
}
impl std::error::Error for FactError {}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn canonical_json(value: &Value) -> String {
    // serde_json::Value objects preserve insertion order; canonicalisation
    // for digest purposes sorts keys recursively.
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), sort(v)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap_or_default()
}

impl TypedFact {
    /// Compute `payload_sha256` and `fact_id`. Must be called after all
    /// mutable fields are final; `seal` is idempotent.
    pub fn seal(&mut self) {
        let payload_digest = self
            .payload
            .fields
            .as_ref()
            .map(|f| sha256_hex(canonical_json(f).as_bytes()))
            .unwrap_or_default();
        let identity = serde_json::json!({
            "device_id": self.subject.device_id,
            "namespace": self.subject.namespace,
            "vrf": self.subject.vrf,
            "fact_kind": self.fact_kind,
            "payload_sha256": payload_digest,
            "capture_id": self.source.capture_id,
            "collector_id": self.source.collector_id,
        });
        self.fact_id = sha256_hex(canonical_json(&identity).as_bytes());
    }

    /// Structural validation. Does not check temporal ordering beyond
    /// internal consistency; consumers own freshness policy.
    pub fn validate(&self) -> Result<(), FactError> {
        if self.schema_version == 0 || self.schema_version > MAX_SUPPORTED_SCHEMA_VERSION {
            return Err(FactError::UnsupportedSchemaVersion(self.schema_version));
        }
        for (name, v) in [
            ("fact_id", &self.fact_id),
            ("fact_kind", &self.fact_kind),
            ("subject.device_id", &self.subject.device_id),
            ("subject.namespace", &self.subject.namespace),
            ("source.collector_id", &self.source.collector_id),
            ("source.source_kind", &self.source.source_kind),
            ("source.capture_id", &self.source.capture_id),
            ("observed_at", &self.observed_at),
        ] {
            if v.is_empty() {
                return Err(FactError::MissingField(name));
            }
        }
        if self.fact_id.len() != 64 || !self.fact_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FactError::InvalidField("fact_id"));
        }
        // Unknown/absent/invalid must stay explicit: a payload with no
        // fields must carry a PayloadState explaining why.
        if self.payload.fields.is_none() && self.payload.state.is_none() {
            return Err(FactError::InvalidField(
                "payload.state required when payload.fields absent",
            ));
        }
        if self.payload.fields.is_some() && self.payload.state.is_some() {
            return Err(FactError::InvalidField(
                "payload.state must be absent when fields present",
            ));
        }
        match self.lifecycle {
            Lifecycle::Withdrawn => {
                if self.withdrawn_reason.as_deref().unwrap_or("").is_empty() {
                    return Err(FactError::LifecycleViolation(
                        "withdrawn facts require withdrawn_reason",
                    ));
                }
            }
            Lifecycle::Superseded => {
                if self.superseded_by.as_deref().unwrap_or("").is_empty() {
                    return Err(FactError::LifecycleViolation(
                        "superseded facts require superseded_by",
                    ));
                }
            }
            Lifecycle::Active | Lifecycle::Stale => {}
        }
        // An authoritative-trust fact cannot be built on unknown-quality
        // payload output.
        if self.trust == TrustClass::Authoritative
            && matches!(
                self.payload.state,
                Some(PayloadState::UnknownQuality | PayloadState::UnstructuredFallback)
            )
        {
            return Err(FactError::TrustViolation(
                "authoritative trust requires structured payload",
            ));
        }
        Ok(())
    }

    /// Schema-evolution gate: deserialize then validate version support.
    /// Returns `UnsupportedSchemaVersion` for anything newer than this
    /// binary understands — callers must route through an explicit upgrade.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, FactError> {
        let fact: TypedFact =
            serde_json::from_slice(bytes).map_err(|_| FactError::InvalidField("json"))?;
        if fact.schema_version > MAX_SUPPORTED_SCHEMA_VERSION {
            return Err(FactError::UnsupportedSchemaVersion(fact.schema_version));
        }
        fact.validate()?;
        Ok(fact)
    }

    pub fn to_vec(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Withdraw this fact in place: lifecycle transition with mandatory
    /// reason. Returns a new fact id is NOT assigned — withdrawal is a
    /// lifecycle annotation on the same identity.
    pub fn withdraw(mut self, reason: impl Into<String>) -> Result<Self, FactError> {
        if self.lifecycle != Lifecycle::Active && self.lifecycle != Lifecycle::Stale {
            return Err(FactError::LifecycleViolation(
                "only active/stale facts can be withdrawn",
            ));
        }
        self.lifecycle = Lifecycle::Withdrawn;
        self.withdrawn_reason = Some(reason.into());
        self.validate()?;
        Ok(self)
    }

    /// Mark superseded by another fact id.
    pub fn supersede(mut self, by: impl Into<String>) -> Result<Self, FactError> {
        if self.lifecycle != Lifecycle::Active {
            return Err(FactError::LifecycleViolation(
                "only active facts can be superseded",
            ));
        }
        self.lifecycle = Lifecycle::Superseded;
        self.superseded_by = Some(by.into());
        self.validate()?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_fact() -> TypedFact {
        let mut f = TypedFact {
            schema_version: SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: "bgp.neighbor.summary".into(),
            subject: FactSubject {
                device_id: "edge-rtr-01".into(),
                namespace: "routing".into(),
                vrf: Some("default".into()),
            },
            source: FactSource {
                collector_id: "collector-a".into(),
                source_kind: "cli_ssh".into(),
                capture_id: "cap-0001".into(),
                session_epoch: 3,
                sequence: 42,
            },
            observed_at: "2026-11-24T10:00:00Z".into(),
            valid_from: Some("2026-11-24T09:59:00Z".into()),
            valid_until: None,
            payload: FactPayload {
                produced_by: "genie".into(),
                fields: Some(serde_json::json!({"neighbors": 2})),
                state: None,
            },
            quality: Quality {
                completeness: Completeness::Complete,
                missing_fields: vec![],
                notes: vec![],
            },
            trust: TrustClass::DeviceAsserted,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            evidence: vec![EvidenceRef {
                kind: "raw_output".into(),
                sha256: "a".repeat(64),
                locator: None,
            }],
        };
        f.seal();
        f
    }

    #[test]
    fn seal_produces_stable_fact_id() {
        let a = sample_fact();
        let b = sample_fact();
        assert_eq!(a.fact_id, b.fact_id);
        assert_eq!(a.fact_id.len(), 64);
        a.validate().unwrap();
    }

    #[test]
    fn fact_id_changes_with_payload() {
        let mut a = sample_fact();
        let mut b = sample_fact();
        b.payload.fields = Some(serde_json::json!({"neighbors": 3}));
        b.seal();
        assert_ne!(a.fact_id, b.fact_id);
        a.payload.fields = Some(serde_json::json!({"neighbors": 3}));
        a.seal();
        assert_eq!(a.fact_id, b.fact_id);
    }

    #[test]
    fn payload_state_required_when_fields_absent() {
        let mut f = sample_fact();
        f.payload.fields = None;
        f.payload.state = None;
        f.seal();
        assert!(matches!(f.validate(), Err(FactError::InvalidField(_))));
        f.payload.state = Some(PayloadState::Unsupported);
        f.validate().unwrap();
    }

    #[test]
    fn state_and_fields_are_mutually_exclusive() {
        let mut f = sample_fact();
        f.payload.state = Some(PayloadState::Empty);
        assert!(f.validate().is_err());
    }

    #[test]
    fn withdraw_requires_reason_and_active_lifecycle() {
        let f = sample_fact();
        let w = f.clone().withdraw("device decommissioned").unwrap();
        assert_eq!(w.lifecycle, Lifecycle::Withdrawn);
        assert!(w.validate().is_ok());
        // Double withdrawal rejected.
        assert!(w.withdraw("again").is_err());
    }

    #[test]
    fn withdrawn_without_reason_fails_validation() {
        let mut f = sample_fact();
        f.lifecycle = Lifecycle::Withdrawn;
        f.withdrawn_reason = None;
        assert!(matches!(
            f.validate(),
            Err(FactError::LifecycleViolation(_))
        ));
    }

    #[test]
    fn supersede_requires_target() {
        let f = sample_fact();
        let s = f.supersede("a".repeat(64)).unwrap();
        assert_eq!(s.lifecycle, Lifecycle::Superseded);
        let mut bad = sample_fact();
        bad.lifecycle = Lifecycle::Superseded;
        bad.superseded_by = None;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn authoritative_trust_rejects_unstructured_payload() {
        let mut f = sample_fact();
        f.payload.fields = None;
        f.payload.state = Some(PayloadState::UnstructuredFallback);
        f.trust = TrustClass::Authoritative;
        f.seal();
        assert!(matches!(f.validate(), Err(FactError::TrustViolation(_))));
        f.trust = TrustClass::DeviceAsserted;
        f.validate().unwrap();
    }

    #[test]
    fn authority_eligibility_boundary() {
        assert!(!TrustClass::Unverified.authority_eligible());
        assert!(!TrustClass::DeviceAsserted.authority_eligible());
        assert!(TrustClass::Corroborated.authority_eligible());
        assert!(TrustClass::Authoritative.authority_eligible());
    }

    #[test]
    fn schema_version_gate_rejects_future_versions() {
        let mut f = sample_fact();
        f.schema_version = MAX_SUPPORTED_SCHEMA_VERSION + 1;
        f.seal();
        assert!(matches!(
            TypedFact::from_slice(&f.to_vec().unwrap()),
            Err(FactError::UnsupportedSchemaVersion(_))
        ));
        f.schema_version = 0;
        assert!(matches!(
            f.validate(),
            Err(FactError::UnsupportedSchemaVersion(0))
        ));
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let f = sample_fact();
        let bytes = f.to_vec().unwrap();
        let back = TypedFact::from_slice(&bytes).unwrap();
        assert_eq!(back.fact_id, f.fact_id);
        assert_eq!(back.trust, f.trust);
        assert_eq!(back.quality.completeness, Completeness::Complete);
    }

    #[test]
    fn unknown_completeness_is_explicit() {
        let mut f = sample_fact();
        f.quality.completeness = Completeness::Unknown;
        f.validate().unwrap(); // unknown is representable, not an error
        let v: Value = serde_json::to_value(&f.quality).unwrap();
        assert_eq!(v["completeness"], "unknown");
    }

    /// Property-style: for any payload fields value, seal+roundtrip is stable
    /// and validation passes for a well-formed fact.
    #[test]
    fn seal_roundtrip_stable_across_payload_shapes() {
        let shapes = [
            serde_json::json!({"a": 1}),
            serde_json::json!({"nested": {"b": [1, 2, {"c": null}]}}),
            serde_json::json!([1, "x", true]),
            serde_json::json!("scalar"),
            serde_json::json!(42),
        ];
        for (i, fields) in shapes.iter().enumerate() {
            let mut f = sample_fact();
            f.payload.fields = Some(fields.clone());
            f.seal();
            let bytes = f.to_vec().unwrap();
            let back = TypedFact::from_slice(&bytes).unwrap();
            assert_eq!(back.fact_id, f.fact_id, "shape {i}");
        }
    }

    /// Regenerate the committed serialized fixture artifact. Run with:
    ///   cargo test -p bonsai --lib typed_facts -- --ignored emit_fixture
    #[test]
    #[ignore = "artifact generator; run explicitly to regenerate fixture"]
    fn emit_fixture_artifact() {
        let fact = sample_fact();
        let bytes = fact.to_vec().unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/bn024/typed-facts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("v1.fixture.json"), &bytes).unwrap();
    }

    /// The committed fixture must deserialize and validate under v1 rules.
    #[test]
    fn committed_fixture_is_valid() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/bn024/typed-facts/v1.fixture.json");
        let bytes = std::fs::read(&path).expect("fixture artifact missing");
        let fact = TypedFact::from_slice(&bytes).expect("fixture must validate");
        assert_eq!(fact.schema_version, SCHEMA_VERSION);
        assert_eq!(fact.fact_kind, "bgp.neighbor.summary");
    }

    /// Property-style: lifecycle transitions are monotonic — once withdrawn
    /// or superseded, no further transition is permitted.
    #[test]
    fn lifecycle_transitions_are_terminal() {
        let f = sample_fact();
        let w = f.clone().withdraw("r").unwrap();
        assert!(w.clone().supersede("x").is_err());
        assert!(w.withdraw("r2").is_err());
        let s = f.supersede("b".repeat(64)).unwrap();
        assert!(s.clone().withdraw("r").is_err());
        assert!(s.supersede("c".repeat(64)).is_err());
    }
}

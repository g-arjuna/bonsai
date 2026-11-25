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
    /// Protocol-level exporter identity (e.g. BMP exporter claim, gNMI
    /// target, SSH host key fingerprint reference). Evidence only —
    /// never canonical device identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exporter_id: Option<String>,
}

/// Subject the fact is about. `device_id` is the canonical inventory device
/// identity (never a raw IP or management address); the remaining fields
/// are independent identity dimensions — do not collapse them into one
/// free-form string:
/// - `scope`: canonical tenant/environment identity scope (`None` =
///   default scope, matching `telemetry_identity` semantics).
/// - `namespace`: fact-domain namespace ("interface", "bgp", ...).
/// - `network_namespace`: ASIC/network namespace the entity occupies
///   (SONiC "asic-0"/"asic-1"/"host", FRR "default", ...). Independent of
///   `namespace`.
/// - `entity`: entity-specific identity within the namespace (interface
///   name, prefix, neighbor address, ...).
/// - `vrf`: routing-instance scoping (PeerKey semantics).
/// - `afi_safi`: routing family for NLRI-bearing facts.
/// - `plane`: truth plane — "operational" | "configured" | "effective" |
///   "startup" | ... Configured/effective/observed state must never be
///   merged into one property.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactSubject {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afi_safi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<String>,
}

// ── Payload & quality ───────────────────────────────────────────────────────

/// Typed payload wrapper. `fields` is the structured fact body; absent or
/// invalid content is represented explicitly, never as an empty success.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FactPayload {
    /// Parser/backend family that produced the payload (e.g. "genie",
    /// "textfsm", "native_sonic_state_db") — provenance, not authority.
    pub produced_by: String,
    /// Parser implementation revision: template version, code version or
    /// content digest identifying the exact extraction logic. Required —
    /// a bare producer name is not an interpretation identity.
    #[serde(default)]
    pub parser_revision: String,
    /// Mapping/profile revision applied after parsing (normalization
    /// ruleset version). `None` = no mapping layer recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_revision: Option<String>,
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
    /// The source capture itself failed, timed out, was unavailable or
    /// was refused — no usable observation exists. Distinct from
    /// `Empty` (capture succeeded and legitimately contained no
    /// records) and `InvalidInput` (input rejected before parsing).
    CaptureFailed,
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

/// Trust class — authority *eligibility claim*, deliberately separate from
/// parse quality. A structured parse from an unauthenticated capture is
/// still only `DeviceAsserted`; nothing here writes to the graph.
///
/// IMPORTANT: this enum is a *claim carried by the fact*, not verified
/// authority. Serialized `corroborated`/`authoritative` values are evidence
/// claims only — they never satisfy [`TypedFact::authority_eligible`].
/// Actual eligibility requires an [`AuthorityGrant`] derived from a trusted
/// Core identity resolution in this process.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// No trust basis established.
    Unverified = 0,
    /// Device/parser asserted the value; transport/authenticity unproven.
    DeviceAsserted = 1,
    /// Corroborated by a second independent observation path.
    Corroborated = 2,
    /// Claimed eligible for authoritative projection.
    Authoritative = 3,
}

impl TrustClass {
    /// Whether this *class claim* asserts authority eligibility. This is
    /// not an authorization check — use [`TypedFact::authority_eligible`],
    /// which additionally requires a live [`AuthorityGrant`].
    pub fn authority_eligible(self) -> bool {
        matches!(self, TrustClass::Corroborated | TrustClass::Authoritative)
    }
}

/// Verified authority grant — the only path by which a fact may be
/// authority-eligible.
///
/// Mirrors the `VerifiedIngressPrincipal` trust boundary
/// (`telemetry_identity.rs`): the attestation flag is never serialized, so
/// a fact restored from JSON, journal or API payload deserializes with
/// `attested = false` and can never re-authorize itself. Consumers must
/// re-derive the grant from a live trusted Core `IdentityResolution`.
///
/// A grant is *bound*: it records the exact subject identity scope,
/// source/exporter and observation axes it was issued for. Attaching it
/// to a different device, tenant scope, collector, capture or sequence —
/// or mutating the fact after issuance — makes
/// [`TypedFact::authority_eligible`] return `false` and fails
/// [`TypedFact::validate`].
///
/// A grant records *that* trusted resolution permitted authority for this
/// fact's subject; it does not itself assert parse quality, freshness or
/// semantic correctness — those remain separate dimensions on the fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuthorityGrant {
    /// Identity status that justified the grant (always `resolved`).
    pub identity_status: crate::telemetry_identity::IdentityStatus,
    /// Canonical device id the resolution produced (evidence, not identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_device_id: Option<String>,
    /// Canonical exporter id the resolution produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_exporter_id: Option<String>,
    /// Identity scope the resolution was produced under.
    pub identity_scope: String,
    // ── Bound fact axes (grant is valid only for this exact fact) ──
    pub subject_device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_scope: Option<String>,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afi_safi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<String>,
    pub collector_id: String,
    pub source_kind: String,
    pub capture_id: String,
    pub session_epoch: u64,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exporter_id: Option<String>,
    /// Set only by [`Self::for_fact`]; skipped by serde so deserialized
    /// grants are always unattested.
    #[serde(skip)]
    attested: bool,
}

impl AuthorityGrant {
    /// Issue a grant for `fact` from a trusted Core identity resolution.
    /// Returns `None` unless ALL of the following hold:
    /// - the resolution was produced by [`crate::telemetry_identity::IdentityResolver`]
    ///   against trusted bindings (`is_resolver_attested`) — a publicly
    ///   constructed `IdentityResolution`, even with `status: Resolved`,
    ///   can never mint a grant;
    /// - the resolution status may update authoritative state;
    /// - the resolved canonical device id equals the fact's subject
    ///   device id, and the resolution scope equals the fact's subject
    ///   scope (default scope when unset);
    /// - the fact's observation source axes are corroborated by the
    ///   resolution's observed envelope: `collector_id` and
    ///   `session_epoch` must be established there and equal the fact's,
    ///   and a fact asserting `exporter_id` must name the resolution's
    ///   canonical exporter. `capture_id`/`sequence` have no
    ///   resolution-side analogue and remain bind-time axes only.
    ///
    /// Callers must obtain the resolution from `IdentityResolver`
    /// (including the verified-ingress path where transport attestation
    /// applies), not from self-reported fact fields.
    pub fn for_fact(
        resolution: &crate::telemetry_identity::IdentityResolution,
        fact: &TypedFact,
    ) -> Option<Self> {
        use crate::telemetry_identity::DEFAULT_IDENTITY_SCOPE;
        if !resolution.is_resolver_attested() || !resolution.may_update_authoritative_state() {
            return None;
        }
        let device_id = resolution.device_id.clone()?;
        if device_id != fact.subject.device_id {
            return None;
        }
        let fact_scope = fact
            .subject
            .scope
            .as_deref()
            .unwrap_or(DEFAULT_IDENTITY_SCOPE);
        if resolution.observed.resolution_scope() != fact_scope {
            return None;
        }
        // Source-axis corroboration: the grant may only be issued for a
        // fact whose observation source is the SAME source the
        // resolution was produced from. `collector_id` and
        // `session_epoch` must be established on the observed envelope
        // and equal the fact's axes; a fact asserting an `exporter_id`
        // must name the resolution's canonical exporter. `capture_id`
        // and `sequence` have no resolution-side analogue — they remain
        // grant-bound axes (post-issuance mutation breaks `binds()`)
        // but cannot be independently corroborated here. This helper is
        // eligibility evidence only: final authorization, freshness and
        // projection policy remain with the trusted consumer boundary.
        if resolution.observed.collector_id.as_deref() != Some(fact.source.collector_id.as_str()) {
            return None;
        }
        if resolution.observed.session_epoch != Some(fact.source.session_epoch) {
            return None;
        }
        if let Some(fact_exporter) = &fact.source.exporter_id
            && resolution.canonical_exporter_id.as_deref() != Some(fact_exporter.as_str())
        {
            return None;
        }
        Some(Self {
            identity_status: resolution.status,
            resolved_device_id: resolution.device_id.clone(),
            resolved_exporter_id: resolution.canonical_exporter_id.clone(),
            identity_scope: resolution.observed.resolution_scope().to_string(),
            subject_device_id: fact.subject.device_id.clone(),
            subject_scope: fact.subject.scope.clone(),
            namespace: fact.subject.namespace.clone(),
            network_namespace: fact.subject.network_namespace.clone(),
            entity: fact.subject.entity.clone(),
            vrf: fact.subject.vrf.clone(),
            afi_safi: fact.subject.afi_safi.clone(),
            plane: fact.subject.plane.clone(),
            collector_id: fact.source.collector_id.clone(),
            source_kind: fact.source.source_kind.clone(),
            capture_id: fact.source.capture_id.clone(),
            session_epoch: fact.source.session_epoch,
            sequence: fact.source.sequence,
            exporter_id: fact.source.exporter_id.clone(),
            attested: true,
        })
    }

    /// Whether this grant was derived from a live trusted resolution in
    /// this process. Always `false` for deserialized values.
    pub fn is_attested(&self) -> bool {
        self.attested
    }

    /// Whether every bound axis still matches `fact`. Any mutation of
    /// subject identity scope, entity axes or observation source after
    /// issuance breaks the binding.
    fn binds(&self, fact: &TypedFact) -> bool {
        use crate::telemetry_identity::DEFAULT_IDENTITY_SCOPE;
        self.subject_device_id == fact.subject.device_id
            && self
                .subject_scope
                .as_deref()
                .unwrap_or(DEFAULT_IDENTITY_SCOPE)
                == fact
                    .subject
                    .scope
                    .as_deref()
                    .unwrap_or(DEFAULT_IDENTITY_SCOPE)
            && self.namespace == fact.subject.namespace
            && self.network_namespace == fact.subject.network_namespace
            && self.entity == fact.subject.entity
            && self.vrf == fact.subject.vrf
            && self.afi_safi == fact.subject.afi_safi
            && self.plane == fact.subject.plane
            && self.collector_id == fact.source.collector_id
            && self.source_kind == fact.source.source_kind
            && self.capture_id == fact.source.capture_id
            && self.session_epoch == fact.source.session_epoch
            && self.sequence == fact.source.sequence
            && self.exporter_id == fact.source.exporter_id
    }
}

impl<'de> Deserialize<'de> for AuthorityGrant {
    /// Deserialization preserves the resolution evidence but NEVER the
    /// attestation — a grant loaded from stored data cannot authorize
    /// authority eligibility.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Repr {
            identity_status: crate::telemetry_identity::IdentityStatus,
            #[serde(default)]
            resolved_device_id: Option<String>,
            #[serde(default)]
            resolved_exporter_id: Option<String>,
            identity_scope: String,
            subject_device_id: String,
            #[serde(default)]
            subject_scope: Option<String>,
            namespace: String,
            #[serde(default)]
            network_namespace: Option<String>,
            #[serde(default)]
            entity: Option<String>,
            #[serde(default)]
            vrf: Option<String>,
            #[serde(default)]
            afi_safi: Option<String>,
            #[serde(default)]
            plane: Option<String>,
            collector_id: String,
            source_kind: String,
            capture_id: String,
            session_epoch: u64,
            sequence: u64,
            #[serde(default)]
            exporter_id: Option<String>,
        }
        let repr = Repr::deserialize(deserializer)?;
        Ok(Self {
            identity_status: repr.identity_status,
            resolved_device_id: repr.resolved_device_id,
            resolved_exporter_id: repr.resolved_exporter_id,
            identity_scope: repr.identity_scope,
            subject_device_id: repr.subject_device_id,
            subject_scope: repr.subject_scope,
            namespace: repr.namespace,
            network_namespace: repr.network_namespace,
            entity: repr.entity,
            vrf: repr.vrf,
            afi_safi: repr.afi_safi,
            plane: repr.plane,
            collector_id: repr.collector_id,
            source_kind: repr.source_kind,
            capture_id: repr.capture_id,
            session_epoch: repr.session_epoch,
            sequence: repr.sequence,
            exporter_id: repr.exporter_id,
            attested: false,
        })
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

#[derive(Clone, Debug, Serialize)]
pub struct TypedFact {
    pub schema_version: u32,
    /// Stable identity: SHA-256 over the canonical combination of entity
    /// key, observation key, interpretation identity, semantic payload
    /// digest and evidence digest — assigned by `seal()` and re-verified
    /// by `validate()`. Lifecycle transitions do NOT change it.
    pub fact_id: String,
    /// Dotted fact kind, e.g. "bgp.neighbor.summary", "interface.state".
    pub fact_kind: String,
    pub subject: FactSubject,
    pub source: FactSource,
    /// RFC-3339 observation timestamp (capture/device-observation time).
    pub observed_at: String,
    /// RFC-3339 time Bonsai ingested/recorded the fact. `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingested_at: Option<String>,
    /// Validity window (RFC-3339); `None` = validity unknown, not infinite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    pub payload: FactPayload,
    /// Semantic content digest: SHA-256 over the canonical payload —
    /// `fields` XOR explicit `state`, plus nothing else. Assigned by
    /// `seal()`; `validate()` recomputes it, so post-seal mutation of the
    /// payload is detected. This is the meaningful normalized content,
    /// distinct from the raw-evidence digests in `evidence`.
    pub payload_sha256: String,
    /// Digest over the canonical `evidence` reference list — binds the
    /// fact to its supporting observation content digests.
    pub evidence_sha256: String,
    pub quality: Quality,
    /// Claimed trust class — an evidence claim, never self-verified
    /// authority. See [`AuthorityGrant`] for the eligibility boundary.
    pub trust: TrustClass,
    /// Verified authority grant derived from a trusted Core identity
    /// resolution in this process, bound to this exact subject/source.
    /// Absent = not authority-eligible. Deserialized grants carry
    /// `attested = false` and confer nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityGrant>,
    pub lifecycle: Lifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// RFC-3339 timestamp of the lifecycle transition. REQUIRED for
    /// `withdrawn`/`superseded` (the promised termination time), forbidden
    /// for `active`, optional for `stale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_changed_at: Option<String>,
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

/// Compute the semantic payload digest: SHA-256 over the canonical JSON
/// of `{fields, state}` — the meaningful normalized content plus the
/// explicit no-data state. Facts with no fields therefore still differ by
/// `PayloadState` (empty vs invalid_input vs unsupported vs ...).
fn semantic_payload_digest(payload: &FactPayload) -> String {
    let body = serde_json::json!({
        "fields": payload.fields.clone().unwrap_or(Value::Null),
        "state": payload.state,
    });
    sha256_hex(canonical_json(&body).as_bytes())
}

/// Digest over the canonical evidence reference list.
fn evidence_digest(evidence: &[EvidenceRef]) -> String {
    let body = serde_json::to_value(evidence).unwrap_or(Value::Null);
    sha256_hex(canonical_json(&body).as_bytes())
}

/// The canonical fact-identity tuple. Combines five independent
/// dimensions so that a new observation, a new interpretation, a
/// different entity scope or a lifecycle annotation are each
/// distinguishable:
/// - entity key: subject identity axes (device, scope, namespace,
///   network namespace, entity, vrf, afi/safi, truth plane)
/// - observation key: source axes (collector, exporter, kind, capture,
///   session epoch, sequence)
/// - interpretation identity: fact kind + producer + parser/mapping
///   revisions + schema version
/// - semantic content digest + evidence digest
fn fact_identity(fact: &TypedFact, payload_sha256: &str, evidence_sha256: &str) -> Value {
    serde_json::json!({
        "schema_version": fact.schema_version,
        "fact_kind": fact.fact_kind,
        "entity": {
            "device_id": fact.subject.device_id,
            "scope": fact.subject.scope,
            "namespace": fact.subject.namespace,
            "network_namespace": fact.subject.network_namespace,
            "entity": fact.subject.entity,
            "vrf": fact.subject.vrf,
            "afi_safi": fact.subject.afi_safi,
            "plane": fact.subject.plane,
        },
        "observation": {
            "collector_id": fact.source.collector_id,
            "source_kind": fact.source.source_kind,
            "capture_id": fact.source.capture_id,
            "session_epoch": fact.source.session_epoch,
            "sequence": fact.source.sequence,
            "exporter_id": fact.source.exporter_id,
        },
        "interpretation": {
            "produced_by": fact.payload.produced_by,
            "parser_revision": fact.payload.parser_revision,
            "mapping_revision": fact.payload.mapping_revision,
        },
        "payload_sha256": payload_sha256,
        "evidence_sha256": evidence_sha256,
    })
}

fn is_rfc3339(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// Payload states that can never support an `authoritative` trust claim
/// or a `complete` completeness assertion.
fn is_degraded_state(state: PayloadState) -> bool {
    matches!(
        state,
        PayloadState::InvalidInput
            | PayloadState::Unsupported
            | PayloadState::UnstructuredFallback
            | PayloadState::UnknownQuality
            | PayloadState::CaptureFailed
    )
}

/// Fact kinds with a registered typed payload validator. Unregistered
/// kinds still require an object-shaped `fields` payload — the generic
/// envelope never implies arbitrary JSON is semantically valid.
fn validate_kind_payload(fact_kind: &str, fields: &Value) -> Result<(), FactError> {
    match fact_kind {
        crate::interface_facts::FACT_KIND_INTERFACE_STATE => {
            crate::interface_facts::validate_payload(fields)
        }
        _ => Ok(()),
    }
}

impl TypedFact {
    /// Compute `payload_sha256`, `evidence_sha256` and `fact_id`. Must be
    /// called after all mutable fields are final; `seal` is idempotent.
    pub fn seal(&mut self) {
        self.payload_sha256 = semantic_payload_digest(&self.payload);
        self.evidence_sha256 = evidence_digest(&self.evidence);
        let identity = fact_identity(self, &self.payload_sha256, &self.evidence_sha256);
        self.fact_id = sha256_hex(canonical_json(&identity).as_bytes());
    }

    /// Structural + integrity validation. Recomputes all digests and the
    /// fact id, so any post-seal mutation is detected; consumers own
    /// freshness policy.
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
            ("payload.produced_by", &self.payload.produced_by),
            ("payload.parser_revision", &self.payload.parser_revision),
            ("payload_sha256", &self.payload_sha256),
            ("evidence_sha256", &self.evidence_sha256),
        ] {
            if v.is_empty() {
                return Err(FactError::MissingField(name));
            }
        }
        if self.fact_id.len() != 64 || !self.fact_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FactError::InvalidField("fact_id"));
        }
        // Timestamp syntax: observed_at is required; optional timestamps
        // must parse when present. Ingestion time is NOT required to be
        // after observation time — device clock skew and replay are
        // legitimate; consumers own freshness policy.
        if !is_rfc3339(&self.observed_at) {
            return Err(FactError::InvalidField("observed_at"));
        }
        for (name, ts) in [
            ("ingested_at", &self.ingested_at),
            ("valid_from", &self.valid_from),
            ("valid_until", &self.valid_until),
            ("lifecycle_changed_at", &self.lifecycle_changed_at),
        ] {
            if let Some(t) = ts
                && !is_rfc3339(t)
            {
                return Err(FactError::InvalidField(name));
            }
        }
        if let (Some(from), Some(until)) = (&self.valid_from, &self.valid_until) {
            let f = chrono::DateTime::parse_from_rfc3339(from)
                .map_err(|_| FactError::InvalidField("valid_from"))?;
            let u = chrono::DateTime::parse_from_rfc3339(until)
                .map_err(|_| FactError::InvalidField("valid_until"))?;
            if f > u {
                return Err(FactError::InvalidField("valid_from > valid_until"));
            }
        }
        // Digest integrity: recompute and compare — detects mutation or
        // an unsealed/forged fact.
        let expected_payload = semantic_payload_digest(&self.payload);
        if self.payload_sha256 != expected_payload {
            return Err(FactError::InvalidField("payload_sha256"));
        }
        let expected_evidence = evidence_digest(&self.evidence);
        if self.evidence_sha256 != expected_evidence {
            return Err(FactError::InvalidField("evidence_sha256"));
        }
        let identity = fact_identity(self, &self.payload_sha256, &self.evidence_sha256);
        let expected_id = sha256_hex(canonical_json(&identity).as_bytes());
        if self.fact_id != expected_id {
            return Err(FactError::InvalidField("fact_id"));
        }
        // Unknown/absent/invalid must stay explicit: a payload with no
        // fields must carry a PayloadState explaining why, and vice versa.
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
        // Structured fields must be a JSON object (a typed record), never
        // a scalar/array/null masquerading as a fact body.
        if let Some(fields) = &self.payload.fields
            && !fields.is_object()
        {
            return Err(FactError::InvalidField("payload.fields"));
        }
        // Registered fact kinds enforce their typed payload contract.
        if let Some(fields) = &self.payload.fields {
            validate_kind_payload(&self.fact_kind, fields)?;
        }
        // A degraded/failed payload can never claim complete coverage.
        if let Some(state) = self.payload.state
            && is_degraded_state(state)
            && self.quality.completeness == Completeness::Complete
        {
            return Err(FactError::InvalidField(
                "degraded payload state cannot claim complete coverage",
            ));
        }
        for ev in &self.evidence {
            if ev.kind.is_empty() {
                return Err(FactError::MissingField("evidence.kind"));
            }
            if ev.sha256.len() != 64 || !ev.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(FactError::InvalidField("evidence.sha256"));
            }
        }
        match self.lifecycle {
            Lifecycle::Withdrawn => {
                if self.withdrawn_reason.as_deref().unwrap_or("").is_empty() {
                    return Err(FactError::LifecycleViolation(
                        "withdrawn facts require withdrawn_reason",
                    ));
                }
                if self.lifecycle_changed_at.is_none() {
                    return Err(FactError::LifecycleViolation(
                        "withdrawn facts require lifecycle_changed_at",
                    ));
                }
            }
            Lifecycle::Superseded => {
                let by = self.superseded_by.as_deref().unwrap_or("");
                if by.is_empty() {
                    return Err(FactError::LifecycleViolation(
                        "superseded facts require superseded_by",
                    ));
                }
                if by.len() != 64 || !by.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(FactError::InvalidField("superseded_by"));
                }
                if self.lifecycle_changed_at.is_none() {
                    return Err(FactError::LifecycleViolation(
                        "superseded facts require lifecycle_changed_at",
                    ));
                }
            }
            Lifecycle::Active => {
                if self.lifecycle_changed_at.is_some() {
                    return Err(FactError::LifecycleViolation(
                        "active facts cannot carry lifecycle_changed_at",
                    ));
                }
            }
            Lifecycle::Stale => {}
        }
        // An authoritative-trust *claim* requires a structured payload —
        // no degraded or absent-fields state may sit under it. Actual
        // authority eligibility is decided by `authority_eligible()`.
        if self.trust == TrustClass::Authoritative && self.payload.fields.is_none() {
            return Err(FactError::TrustViolation(
                "authoritative trust requires structured payload",
            ));
        }
        // A serialized grant is evidence only, but it must still bind to
        // THIS fact's current axes — a grant minted for another device,
        // scope, collector or capture is invalid here regardless of
        // attestation.
        if let Some(grant) = &self.authority
            && !grant.binds(self)
        {
            return Err(FactError::InvalidField("authority"));
        }
        Ok(())
    }

    /// Whether this fact may feed an authoritative projection. Requires
    /// BOTH an authority-eligible trust *claim* AND a live attested
    /// [`AuthorityGrant`] derived from trusted Core identity resolution.
    /// A serialized `trust: "authoritative"` alone never qualifies, and a
    /// deserialized grant is unattested — replayed or API-supplied facts
    /// must have authority re-derived by the consumer.
    ///
    /// This is eligibility metadata only; consumers still own freshness,
    /// completeness, scope and projection policy.
    pub fn authority_eligible(&self) -> bool {
        self.trust.authority_eligible()
            && self.payload.fields.is_some()
            && self
                .authority
                .as_ref()
                .is_some_and(|grant| grant.is_attested() && grant.binds(self))
    }

    /// Schema-evolution gate: deserialize then validate version support.
    /// Returns `UnsupportedSchemaVersion` for anything newer than this
    /// binary understands — callers must route through an explicit upgrade.
    /// The version is checked on the raw JSON first so unsupported
    /// versions are reported distinctly rather than as generic parse
    /// errors (deserialization itself runs full validation).
    pub fn from_slice(bytes: &[u8]) -> Result<Self, FactError> {
        let raw: Value =
            serde_json::from_slice(bytes).map_err(|_| FactError::InvalidField("json"))?;
        if let Some(v) = raw.get("schema_version").and_then(|v| v.as_u64())
            && (v == 0 || v as u32 > MAX_SUPPORTED_SCHEMA_VERSION)
        {
            return Err(FactError::UnsupportedSchemaVersion(v as u32));
        }
        let fact: TypedFact =
            serde_json::from_value(raw).map_err(|_| FactError::InvalidField("json"))?;
        fact.validate()?;
        Ok(fact)
    }

    pub fn to_vec(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Withdraw this fact in place: lifecycle transition with mandatory
    /// reason and termination timestamp. A withdrawal is a lifecycle
    /// annotation on the same identity — `fact_id` does not change.
    pub fn withdraw(
        mut self,
        reason: impl Into<String>,
        changed_at: impl Into<String>,
    ) -> Result<Self, FactError> {
        if self.lifecycle != Lifecycle::Active && self.lifecycle != Lifecycle::Stale {
            return Err(FactError::LifecycleViolation(
                "only active/stale facts can be withdrawn",
            ));
        }
        self.lifecycle = Lifecycle::Withdrawn;
        self.withdrawn_reason = Some(reason.into());
        self.lifecycle_changed_at = Some(changed_at.into());
        self.validate()?;
        Ok(self)
    }

    /// Mark superseded by another fact id, recording when the
    /// supersession took effect.
    pub fn supersede(
        mut self,
        by: impl Into<String>,
        changed_at: impl Into<String>,
    ) -> Result<Self, FactError> {
        if self.lifecycle != Lifecycle::Active {
            return Err(FactError::LifecycleViolation(
                "only active facts can be superseded",
            ));
        }
        self.lifecycle = Lifecycle::Superseded;
        self.superseded_by = Some(by.into());
        self.lifecycle_changed_at = Some(changed_at.into());
        self.validate()?;
        Ok(self)
    }
}

/// Deserialization is a validated boundary: a `TypedFact` cannot be
/// constructed from JSON that fails `validate()` — there is no path that
/// bypasses digest recomputation, timestamp checks, lifecycle invariants
/// or the typed-payload contract.
impl<'de> Deserialize<'de> for TypedFact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Repr {
            schema_version: u32,
            fact_id: String,
            fact_kind: String,
            subject: FactSubject,
            source: FactSource,
            observed_at: String,
            #[serde(default)]
            ingested_at: Option<String>,
            #[serde(default)]
            valid_from: Option<String>,
            #[serde(default)]
            valid_until: Option<String>,
            payload: FactPayload,
            payload_sha256: String,
            evidence_sha256: String,
            quality: Quality,
            trust: TrustClass,
            #[serde(default)]
            authority: Option<AuthorityGrant>,
            lifecycle: Lifecycle,
            #[serde(default)]
            withdrawn_reason: Option<String>,
            #[serde(default)]
            superseded_by: Option<String>,
            #[serde(default)]
            lifecycle_changed_at: Option<String>,
            #[serde(default)]
            evidence: Vec<EvidenceRef>,
        }
        let repr = Repr::deserialize(deserializer)?;
        let fact = Self {
            schema_version: repr.schema_version,
            fact_id: repr.fact_id,
            fact_kind: repr.fact_kind,
            subject: repr.subject,
            source: repr.source,
            observed_at: repr.observed_at,
            ingested_at: repr.ingested_at,
            valid_from: repr.valid_from,
            valid_until: repr.valid_until,
            payload: repr.payload,
            payload_sha256: repr.payload_sha256,
            evidence_sha256: repr.evidence_sha256,
            quality: repr.quality,
            trust: repr.trust,
            authority: repr.authority,
            lifecycle: repr.lifecycle,
            withdrawn_reason: repr.withdrawn_reason,
            superseded_by: repr.superseded_by,
            lifecycle_changed_at: repr.lifecycle_changed_at,
            evidence: repr.evidence,
        };
        fact.validate().map_err(serde::de::Error::custom)?;
        Ok(fact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_identity::{
        BindingProvenance, BindingStatus, IdentityResolver, IdentityStatus,
        TelemetrySourceIdentity, TrustedBinding,
    };
    use std::path::Path;

    const TS: &str = "2026-11-24T11:00:00Z";

    fn sample_fact() -> TypedFact {
        let mut f = TypedFact {
            schema_version: SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: "bgp.neighbor.summary".into(),
            subject: FactSubject {
                device_id: "edge-rtr-01".into(),
                scope: None,
                namespace: "routing".into(),
                network_namespace: None,
                entity: Some("10.0.0.2".into()),
                vrf: Some("default".into()),
                afi_safi: Some("ipv4-unicast".into()),
                plane: Some("operational".into()),
            },
            source: FactSource {
                collector_id: "collector-a".into(),
                source_kind: "cli_ssh".into(),
                capture_id: "cap-0001".into(),
                session_epoch: 3,
                sequence: 42,
                exporter_id: None,
            },
            observed_at: "2026-11-24T10:00:00Z".into(),
            ingested_at: Some("2026-11-24T10:00:01Z".into()),
            valid_from: Some("2026-11-24T09:59:00Z".into()),
            valid_until: None,
            payload: FactPayload {
                produced_by: "genie".into(),
                parser_revision: "genie-parser-26.10".into(),
                mapping_revision: Some("bn024-map-1".into()),
                fields: Some(serde_json::json!({"neighbors": 2})),
                state: None,
            },
            payload_sha256: String::new(),
            evidence_sha256: String::new(),
            quality: Quality {
                completeness: Completeness::Complete,
                missing_fields: vec![],
                notes: vec![],
            },
            trust: TrustClass::DeviceAsserted,
            authority: None,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            lifecycle_changed_at: None,
            evidence: vec![EvidenceRef {
                kind: "raw_output".into(),
                sha256: "a".repeat(64),
                locator: None,
            }],
        };
        f.seal();
        f
    }

    fn interface_fact(namespace: &str) -> TypedFact {
        let mut f = sample_fact();
        f.fact_kind = crate::interface_facts::FACT_KIND_INTERFACE_STATE.into();
        f.subject.namespace = "interface".into();
        f.subject.network_namespace = Some(namespace.into());
        f.subject.entity = Some("Ethernet0".into());
        f.subject.vrf = None;
        f.subject.afi_safi = None;
        f.subject.plane = Some("operational".into());
        f.payload.produced_by = "native_sonic_state_db".into();
        f.payload.parser_revision = "sonic-iface-2026.11".into();
        f.payload.mapping_revision = None;
        f.payload.fields = Some(serde_json::json!({
            "interface_name": "Ethernet0",
            "administrative_state": {"state": "known", "value": "up"},
            "operational_state": {"state": "known", "value": "up"},
            "interface_type": {"state": "known", "value": "physical"},
            "ifindex": {"state": "known", "value": 3},
            "mtu": {"state": "known", "value": 9216},
            "speed": {"state": "unknown"},
            "mac_address": {"state": "unknown"},
        }));
        f.seal();
        f
    }

    /// A real `Resolved` IdentityResolution produced by the trusted
    /// resolver against a provisioned binding — the only kind that can
    /// mint an AuthorityGrant.
    fn resolver_resolution(device_id: &str) -> crate::telemetry_identity::IdentityResolution {
        let binding = TrustedBinding {
            binding_id: "bind-1".into(),
            scope: "default".into(),
            protocol: "cli_ssh".into(),
            exporter_id: "exp-1".into(),
            device_id: device_id.into(),
            match_transport: None,
            match_reported_identity: None,
            match_exporter_claim: None,
            match_observation_domain: None,
            match_ingress_credential: None,
            provenance: BindingProvenance::OperatorProvisioned,
            status: BindingStatus::Active,
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: "test".into(),
            updated_by: "test".into(),
        };
        // The observed envelope must establish the same collector and
        // session axes the sample fact asserts — for_fact now requires
        // source-axis corroboration, not just device/scope identity.
        let observed = TelemetrySourceIdentity::observed("10.0.0.1")
            .with_protocol("cli_ssh")
            .with_collector("collector-a")
            .with_session_epoch(3);
        IdentityResolver::resolve(&observed, &[binding])
    }

    fn unattested_resolved() -> crate::telemetry_identity::IdentityResolution {
        // Publicly constructible path: status says Resolved but the value
        // never crossed IdentityResolver — must not mint authority.
        crate::telemetry_identity::IdentityResolution::unattested(
            IdentityStatus::Resolved,
            TelemetrySourceIdentity::observed("10.0.0.1").with_protocol("cli_ssh"),
        )
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
        f.quality.completeness = Completeness::Unknown;
        f.seal();
        assert!(matches!(f.validate(), Err(FactError::InvalidField(_))));
        f.payload.state = Some(PayloadState::Unsupported);
        f.seal();
        f.validate().unwrap();
    }

    #[test]
    fn degraded_state_cannot_claim_complete_coverage() {
        let mut f = sample_fact();
        f.payload.fields = None;
        f.payload.state = Some(PayloadState::UnstructuredFallback);
        // quality.completeness stays Complete -> must fail.
        f.seal();
        assert!(matches!(f.validate(), Err(FactError::InvalidField(_))));
        f.quality.completeness = Completeness::Partial;
        f.seal();
        f.validate().unwrap();
    }

    #[test]
    fn state_and_fields_are_mutually_exclusive() {
        let mut f = sample_fact();
        f.payload.state = Some(PayloadState::Empty);
        assert!(f.validate().is_err());
    }

    #[test]
    fn payload_states_without_fields_have_distinct_identities() {
        // The core B2 regression: Empty / InvalidInput / Unsupported /
        // UnstructuredFallback / UnknownQuality must NOT share a fact_id.
        let mut ids = std::collections::BTreeSet::new();
        for state in [
            PayloadState::Empty,
            PayloadState::InvalidInput,
            PayloadState::Unsupported,
            PayloadState::UnstructuredFallback,
            PayloadState::UnknownQuality,
        ] {
            let mut f = sample_fact();
            f.payload.fields = None;
            f.payload.state = Some(state);
            f.quality.completeness = Completeness::Unknown;
            f.seal();
            f.validate().unwrap();
            assert!(ids.insert(f.fact_id.clone()), "duplicate id for {state:?}");
            assert_ne!(
                f.payload_sha256,
                semantic_payload_digest(&FactPayload {
                    produced_by: "x".into(),
                    parser_revision: "r".into(),
                    mapping_revision: None,
                    fields: None,
                    state: None,
                }),
                "state must enter the semantic digest"
            );
        }
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn identity_dimensions_are_independent() {
        // Same capture id reused under another session epoch.
        let a = sample_fact();
        let mut b = sample_fact();
        b.source.session_epoch = 4;
        b.seal();
        assert_ne!(a.fact_id, b.fact_id);
        // Same sequence after restart = different epoch (covered above);
        // distinct collector / exporter / capture also differ.
        for mutate in [
            (|f: &mut TypedFact| f.source.collector_id = "collector-b".into())
                as fn(&mut TypedFact),
            (|f: &mut TypedFact| f.source.capture_id = "cap-0002".into()),
            (|f: &mut TypedFact| f.source.exporter_id = Some("exp-9".into())),
            (|f: &mut TypedFact| f.subject.scope = Some("tenant-a".into())),
            (|f: &mut TypedFact| f.subject.vrf = Some("vrf-blue".into())),
            (|f: &mut TypedFact| f.subject.network_namespace = Some("asic-1".into())),
            (|f: &mut TypedFact| f.subject.plane = Some("configured".into())),
        ] {
            let mut x = sample_fact();
            mutate(&mut x);
            x.seal();
            assert_ne!(x.fact_id, a.fact_id);
        }
    }

    #[test]
    fn identical_interface_names_across_asic_namespaces_differ() {
        let a = interface_fact("asic-0");
        let b = interface_fact("asic-1");
        assert_ne!(a.fact_id, b.fact_id);
        // Same namespace, replayed identically -> same id (duplicate replay).
        let c = interface_fact("asic-0");
        assert_eq!(a.fact_id, c.fact_id);
        // Parser revision is part of interpretation identity.
        let mut d = interface_fact("asic-0");
        d.payload.parser_revision = "sonic-iface-2026.12".into();
        d.seal();
        assert_ne!(a.fact_id, d.fact_id);
    }

    #[test]
    fn reordered_json_keys_do_not_change_identity() {
        let mut a = sample_fact();
        // Rebuild fields with reversed insertion order.
        a.payload.fields = Some(serde_json::json!({"z": 1, "a": {"y": 2, "b": 3}}));
        a.seal();
        let mut b = sample_fact();
        b.payload.fields = Some(serde_json::json!({"a": {"b": 3, "y": 2}, "z": 1}));
        b.seal();
        assert_eq!(a.payload_sha256, b.payload_sha256);
        assert_eq!(a.fact_id, b.fact_id);
    }

    #[test]
    fn lifecycle_and_temporal_fields_do_not_change_fact_id() {
        let a = sample_fact();
        let w = a.clone().withdraw("decommissioned", TS).expect("withdraw");
        assert_eq!(a.fact_id, w.fact_id);
        assert_ne!(a.lifecycle, w.lifecycle);
        // Ingestion/observed timestamps are not identity dimensions.
        let mut b = sample_fact();
        b.observed_at = "2026-11-24T10:05:00Z".into();
        b.ingested_at = None;
        b.seal();
        assert_eq!(a.fact_id, b.fact_id);
    }

    #[test]
    fn post_seal_mutation_is_detected() {
        let mut f = sample_fact();
        f.subject.entity = Some(" mutated".into());
        // Without reseal: digest mismatch.
        assert!(matches!(
            f.validate(),
            Err(FactError::InvalidField("fact_id"))
        ));
        // With reseal but stale grant binding: covered by authority tests.
        f.seal();
        f.validate().unwrap();
    }

    #[test]
    fn malformed_timestamps_and_windows_rejected() {
        let mut f = sample_fact();
        f.observed_at = "not-a-time".into();
        f.seal();
        assert!(matches!(
            f.validate(),
            Err(FactError::InvalidField("observed_at"))
        ));
        let mut f = sample_fact();
        f.valid_from = Some("2026-11-24T12:00:00Z".into());
        f.valid_until = Some("2026-11-24T11:00:00Z".into());
        f.seal();
        assert!(f.validate().is_err());
    }

    #[test]
    fn scalar_payload_rejected() {
        let mut f = sample_fact();
        f.payload.fields = Some(serde_json::json!(42));
        f.seal();
        assert!(matches!(
            f.validate(),
            Err(FactError::InvalidField("payload.fields"))
        ));
        f.payload.fields = Some(serde_json::json!("scalar"));
        f.seal();
        assert!(f.validate().is_err());
    }

    #[test]
    fn interface_state_payload_is_typed() {
        let f = interface_fact("asic-0");
        f.validate().unwrap();
        // Corrupt the typed payload -> kind validator rejects.
        let mut bad = interface_fact("asic-0");
        bad.payload.fields = Some(serde_json::json!({"interface_name": ""}));
        bad.seal();
        assert!(bad.validate().is_err());
        // Unknown kind still requires object fields.
        let mut g = sample_fact();
        g.fact_kind = "bgp.neighbor.summary".into();
        g.validate().unwrap();
    }

    #[test]
    fn withdraw_requires_reason_timestamp_and_active_lifecycle() {
        let f = sample_fact();
        let w = f.clone().withdraw("device decommissioned", TS).unwrap();
        assert_eq!(w.lifecycle, Lifecycle::Withdrawn);
        assert_eq!(w.lifecycle_changed_at.as_deref(), Some(TS));
        assert!(w.validate().is_ok());
        assert!(w.clone().withdraw("again", TS).is_err());
        // Withdrawn without recorded termination time fails.
        let mut bad = sample_fact();
        bad.lifecycle = Lifecycle::Withdrawn;
        bad.withdrawn_reason = Some("r".into());
        bad.lifecycle_changed_at = None;
        bad.seal();
        assert!(matches!(
            bad.validate(),
            Err(FactError::LifecycleViolation(_))
        ));
    }

    #[test]
    fn withdrawn_without_reason_fails_validation() {
        let mut f = sample_fact();
        f.lifecycle = Lifecycle::Withdrawn;
        f.withdrawn_reason = None;
        f.lifecycle_changed_at = Some(TS.into());
        f.seal();
        assert!(matches!(
            f.validate(),
            Err(FactError::LifecycleViolation(_))
        ));
    }

    #[test]
    fn supersede_requires_target_and_timestamp() {
        let f = sample_fact();
        let s = f.supersede("a".repeat(64), TS).unwrap();
        assert_eq!(s.lifecycle, Lifecycle::Superseded);
        let mut bad = sample_fact();
        bad.lifecycle = Lifecycle::Superseded;
        bad.superseded_by = Some("not-hex".into());
        bad.lifecycle_changed_at = Some(TS.into());
        bad.seal();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn authoritative_trust_rejects_unstructured_payload() {
        let mut f = sample_fact();
        f.payload.fields = None;
        f.payload.state = Some(PayloadState::UnstructuredFallback);
        f.quality.completeness = Completeness::Partial;
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

    // ── B1: authority grant binding regressions ────────────────────────

    #[test]
    fn grant_requires_resolver_attested_resolution() {
        // A genuine resolver-produced Resolved resolution mints a grant.
        let res = resolver_resolution("edge-rtr-01");
        assert!(res.is_resolver_attested());
        let fact = sample_fact();
        let grant = AuthorityGrant::for_fact(&res, &fact).expect("bound grant");
        assert!(grant.is_attested());
        assert_eq!(grant.resolved_device_id.as_deref(), Some("edge-rtr-01"));

        // A publicly constructed IdentityResolution — even with
        // status Resolved — can never mint a grant.
        let forged = unattested_resolved();
        assert!(!forged.is_resolver_attested());
        assert!(AuthorityGrant::for_fact(&forged, &fact).is_none());

        // Non-resolved statuses from the real resolver cannot mint either.
        let mut binding = TrustedBinding {
            binding_id: "bind-x".into(),
            scope: "default".into(),
            protocol: "cli_ssh".into(),
            exporter_id: "exp-x".into(),
            device_id: "edge-rtr-01".into(),
            match_transport: None,
            match_reported_identity: None,
            match_exporter_claim: None,
            match_observation_domain: None,
            match_ingress_credential: None,
            provenance: BindingProvenance::OperatorProvisioned,
            status: BindingStatus::Revoked, // revoked source authority
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: "t".into(),
            updated_by: "t".into(),
        };
        let observed = TelemetrySourceIdentity::observed("10.0.0.9")
            .with_protocol("cli_ssh")
            .with_collector("collector-a")
            .with_session_epoch(3);
        let res = IdentityResolver::resolve(&observed, std::slice::from_ref(&binding));
        assert_ne!(res.status, IdentityStatus::Resolved);
        assert!(AuthorityGrant::for_fact(&res, &fact).is_none());
        binding.status = BindingStatus::Active;
        // Resolved status without canonical device id cannot occur via
        // the resolver; the unattested constructor covers the forged case.
    }

    #[test]
    fn grant_does_not_transfer_across_subjects() {
        let res = resolver_resolution("edge-rtr-01");
        let fact_a = sample_fact(); // device edge-rtr-01
        let grant_a = AuthorityGrant::for_fact(&res, &fact_a).unwrap();

        // Device B fact: resolution for device A cannot be issued for it.
        let mut fact_b = sample_fact();
        fact_b.subject.device_id = "edge-rtr-02".into();
        fact_b.seal();
        assert!(AuthorityGrant::for_fact(&res, &fact_b).is_none());

        // Attaching A's grant to B's fact fails validation AND eligibility.
        fact_b.authority = Some(grant_a.clone());
        fact_b.seal();
        assert!(matches!(
            fact_b.validate(),
            Err(FactError::InvalidField("authority"))
        ));
        assert!(!fact_b.authority_eligible());

        // Tenant/scope A -> B.
        let mut fact_scope = sample_fact();
        fact_scope.subject.scope = Some("tenant-b".into());
        fact_scope.seal();
        assert!(AuthorityGrant::for_fact(&res, &fact_scope).is_none());
        let mut attached = sample_fact();
        attached.authority = Some(grant_a.clone());
        attached.subject.scope = Some("tenant-b".into());
        attached.seal();
        assert!(attached.validate().is_err());
        assert!(!attached.authority_eligible());

        // Different collector / capture / sequence observation.
        for mutate in [
            (|f: &mut TypedFact| f.source.collector_id = "collector-b".into())
                as fn(&mut TypedFact),
            (|f: &mut TypedFact| f.source.capture_id = "cap-9999".into()),
            (|f: &mut TypedFact| f.source.sequence = 43),
            (|f: &mut TypedFact| f.source.exporter_id = Some("exp-2".into())),
            (|f: &mut TypedFact| f.subject.entity = Some("192.0.2.1".into())),
        ] {
            let mut f = sample_fact();
            mutate(&mut f);
            f.authority = Some(grant_a.clone());
            f.seal();
            assert!(f.validate().is_err(), "mutated fact accepted grant");
            assert!(!f.authority_eligible());
        }
    }

    #[test]
    fn grant_rejects_resolution_from_unrelated_source() {
        // A genuine resolver-attested resolution produced from a
        // DIFFERENT collector/session observation cannot mint a grant
        // for this fact — device/scope identity alone is not enough.
        let binding = TrustedBinding {
            binding_id: "bind-src".into(),
            scope: "default".into(),
            protocol: "cli_ssh".into(),
            exporter_id: "exp-1".into(),
            device_id: "edge-rtr-01".into(),
            match_transport: None,
            match_reported_identity: None,
            match_exporter_claim: None,
            match_observation_domain: None,
            match_ingress_credential: None,
            provenance: BindingProvenance::OperatorProvisioned,
            status: BindingStatus::Active,
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: "t".into(),
            updated_by: "t".into(),
        };
        for (collector, epoch) in [("collector-b", 3u64), ("collector-a", 9u64)] {
            let observed = TelemetrySourceIdentity::observed("10.0.0.7")
                .with_protocol("cli_ssh")
                .with_collector(collector)
                .with_session_epoch(epoch);
            let res = IdentityResolver::resolve(&observed, std::slice::from_ref(&binding));
            assert!(res.is_resolver_attested());
            assert!(AuthorityGrant::for_fact(&res, &sample_fact()).is_none());
        }
        // A fact asserting a different exporter than the resolution's
        // canonical exporter cannot reuse the grant either.
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.source.exporter_id = Some("exp-9".into());
        f.seal();
        assert!(AuthorityGrant::for_fact(&res, &f).is_none());
        // The canonical exporter id itself is accepted.
        let mut f = sample_fact();
        f.source.exporter_id = Some("exp-1".into());
        f.seal();
        assert!(AuthorityGrant::for_fact(&res, &f).is_some());
    }

    #[test]
    fn public_mutation_after_grant_breaks_eligibility() {
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.trust = TrustClass::Corroborated;
        f.authority = AuthorityGrant::for_fact(&res, &f);
        assert!(f.authority_eligible());
        f.seal();
        // Mutate a bound axis and reseal: the grant no longer binds.
        f.subject.vrf = Some("vrf-other".into());
        f.seal();
        assert!(matches!(
            f.validate(),
            Err(FactError::InvalidField("authority"))
        ));
        assert!(!f.authority_eligible());
    }

    #[test]
    fn forged_trust_claim_is_not_authority() {
        let mut f = sample_fact();
        f.trust = TrustClass::Authoritative;
        f.seal();
        let bytes = f.to_vec().unwrap();
        let back = TypedFact::from_slice(&bytes).unwrap();
        assert_eq!(back.trust, TrustClass::Authoritative);
        assert!(!back.authority_eligible());
    }

    #[test]
    fn deserialized_grant_loses_attestation() {
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.trust = TrustClass::Corroborated;
        f.authority = AuthorityGrant::for_fact(&res, &f);
        assert!(f.authority_eligible());
        f.seal();

        let bytes = f.to_vec().unwrap();
        let back = TypedFact::from_slice(&bytes).unwrap();
        // Grant evidence (including bound axes) survives; attestation
        // does not — replayed facts must re-derive authority.
        assert!(back.authority.is_some());
        assert!(!back.authority_eligible());
        // The bound grant still validates because axes match.
        back.validate().unwrap();
    }

    #[test]
    fn degraded_payload_cannot_be_authority_eligible() {
        // Even with a live bound grant, a fact without structured fields
        // is never authority-eligible.
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.payload.fields = None;
        f.payload.state = Some(PayloadState::Empty);
        f.quality.completeness = Completeness::Unknown;
        f.trust = TrustClass::Corroborated;
        f.authority = AuthorityGrant::for_fact(&res, &f);
        f.seal();
        assert!(!f.authority_eligible());
    }

    #[test]
    fn fact_id_binds_schema_version_and_producer() {
        let mut a = sample_fact();
        let mut b = sample_fact();
        b.payload.produced_by = "textfsm".into();
        b.seal();
        assert_ne!(a.fact_id, b.fact_id);
        a.payload.produced_by = "textfsm".into();
        a.seal();
        assert_eq!(a.fact_id, b.fact_id);

        // Interpretation revision is identity, not just producer name.
        let mut c = sample_fact();
        c.payload.parser_revision = "genie-parser-26.11".into();
        c.seal();
        assert_ne!(a.fact_id, c.fact_id);
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
        assert_eq!(back.payload_sha256, f.payload_sha256);
    }

    #[test]
    fn unknown_completeness_is_explicit() {
        let mut f = sample_fact();
        f.quality.completeness = Completeness::Unknown;
        f.validate().unwrap(); // unknown is representable, not an error
        let v: Value = serde_json::to_value(&f.quality).unwrap();
        assert_eq!(v["completeness"], "unknown");
    }

    /// Property-style: for any object payload fields value, seal+roundtrip
    /// is stable and validation passes for a well-formed fact.
    #[test]
    fn seal_roundtrip_stable_across_payload_shapes() {
        let shapes = [
            serde_json::json!({"a": 1}),
            serde_json::json!({"nested": {"b": [1, 2, {"c": null}]}}),
            serde_json::json!({"list": [1, "x", true]}),
            serde_json::json!({"scalar_string": "value"}),
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
        let w = f.clone().withdraw("r", TS).unwrap();
        assert!(w.clone().supersede("x".repeat(64), TS).is_err());
        assert!(w.withdraw("r2", TS).is_err());
        let s = f.supersede("b".repeat(64), TS).unwrap();
        assert!(s.clone().withdraw("r", TS).is_err());
        assert!(s.supersede("c".repeat(64), TS).is_err());
    }
}

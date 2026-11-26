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
use serde_json::{Value, json};
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
/// - `interface_identity`: optional structured refinement for interface
///   subjects. This is not a second identity authority: `entity` remains
///   the existing canonical entity key, `network_namespace` remains the
///   ASIC/namespace axis and `vrf` remains evidence-based network-instance
///   scoping. The refinement exists because dotted vendor names alone cannot
///   safely distinguish base interfaces, logical interfaces and subinterfaces.
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
    /// Structured interface identity refinement. Present only when the
    /// producing normalizer has positive structured evidence; never infer
    /// this object merely by splitting a display name at a dot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_identity: Option<InterfaceSubjectIdentity>,
}

/// Coarse identity class for an interface subject. `Physical` means the
/// base/physical interface itself, `Logical` a logical interface that is not
/// a numbered subinterface, and `Subinterface` a numbered child of a base
/// interface. Absence of `interface_identity` means the refinement is
/// unknown/not supplied, not that the interface is physical.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceIdentityKind {
    Physical,
    Logical,
    Subinterface,
}

/// Structured interface identity refinement for [`FactSubject`].
///
/// Compatibility rule: this object is additive optional schema-v1 data. It
/// is omitted from serialization and fact identity when absent, so existing
/// v1 facts and evidence ids are unchanged. Once present, every field that
/// is present is part of fact identity and authority binding.
///
/// Identity convention for numbered subinterfaces: `subject.entity` is the
/// canonical base-interface key and MUST equal `parent_entity`; the numeric
/// `subinterface_index` distinguishes children (including zero). The exact
/// vendor-native display name (for example `Ethernet1.100`) is preserved
/// independently in `vendor_native_name` and is never parsed to invent these
/// axes. Network-instance attachment remains the separate evidence-based
/// `subject.vrf` axis; ASIC/host placement remains `network_namespace`.
///
/// Canonical entity identity and observation/fact identity are deliberately
/// distinct: the canonical interface entity key is the scoped subject axes
/// plus this structured refinement, while `fact_id` additionally binds the
/// observation, interpretation, payload and evidence. `vendor_native_name`
/// is an alias/display evidence axis and interface kind is classification
/// evidence supplied by the producer; changing either changes the complete
/// fact interpretation identity, but consumers must not use the native alias
/// as a second canonical entity authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceSubjectIdentity {
    pub kind: InterfaceIdentityKind,
    /// Canonical parent/base interface entity when the evidence establishes
    /// one. Required for subinterfaces; optional for logical interfaces;
    /// forbidden for physical base interfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_entity: Option<String>,
    /// Numeric subinterface index. Required for `subinterface` (zero is a
    /// real value and must not be conflated with the physical/base interface),
    /// forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subinterface_index: Option<u32>,
    /// Exact source-native interface display/name evidence. Preserved for
    /// corroboration and operator display; never used as a parsing authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_native_name: Option<String>,
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
    /// Parser ran successfully but produced no typed fields, and the
    /// capture/interpretation contract genuinely establishes that the
    /// decoded population was empty. Never use this for a nonempty input
    /// whose records were rejected, omitted or budget-truncated.
    Empty,
    /// The decoded observation contained records (or emptiness could not
    /// be established), but no usable validated entity fact was produced.
    /// Report counters carry the exact invalid/omitted/rejected reason.
    NoUsableRecords,
    /// Valid decoded output existed, but the effective output-fact budget
    /// prevented every entity fact from being emitted.
    OutputBudgetExhausted,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_identity: Option<InterfaceSubjectIdentity>,
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
    /// Content digest captured at issuance over the grant's bound axes
    /// PLUS the fact's `payload_sha256`/`evidence_sha256`. Every grant
    /// field is public (serialized evidence), so without this digest a
    /// cloned grant could have its bound axes mutated consistently with
    /// a mutated fact and still "bind". Any post-issuance mutation of
    /// grant axes or of the fact's semantic/evidence payload breaks the
    /// match. Skipped by serde — deserialized grants are unattested.
    #[serde(skip)]
    attested_digest: Option<String>,
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
        // The attestation must still cover the resolution's CURRENT
        // fields: `IdentityResolution`'s evidence fields are public, so
        // a cloned-then-mutated resolution (device id, observed scope,
        // collector, session epoch, exporter axes) fails closed here
        // rather than minting a grant for a different source.
        if !resolution.is_resolver_attested()
            || !resolution.attestation_covers_state()
            || !resolution.may_update_authoritative_state()
        {
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
        // The grant is issued for THIS sealed fact's semantic and
        // evidence content, not just its identity axes: mutating
        // `payload.fields` or `evidence` and resealing changes the
        // digests bound below and breaks `binds()`.
        let mut grant = Self {
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
            interface_identity: fact.subject.interface_identity.clone(),
            collector_id: fact.source.collector_id.clone(),
            source_kind: fact.source.source_kind.clone(),
            capture_id: fact.source.capture_id.clone(),
            session_epoch: fact.source.session_epoch,
            sequence: fact.source.sequence,
            exporter_id: fact.source.exporter_id.clone(),
            attested: true,
            attested_digest: None,
        };
        grant.attested_digest = Some(grant_content_digest(&grant, fact));
        Some(grant)
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
        // An ATTESTED grant additionally re-verifies its issuance
        // digest: bound axes plus the fact's semantic/evidence digests.
        // Unattested (deserialized) grants carry no digest and fall back
        // to axis comparison only — they confer no eligibility anyway.
        if self.attested
            && self
                .attested_digest
                .as_deref()
                .is_none_or(|d| d != grant_content_digest(self, fact))
        {
            return false;
        }
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
            && self.interface_identity == fact.subject.interface_identity
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
            #[serde(default)]
            interface_identity: Option<InterfaceSubjectIdentity>,
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
            interface_identity: repr.interface_identity,
            collector_id: repr.collector_id,
            source_kind: repr.source_kind,
            capture_id: repr.capture_id,
            session_epoch: repr.session_epoch,
            sequence: repr.sequence,
            exporter_id: repr.exporter_id,
            attested: false,
            attested_digest: None,
        })
    }
}

/// Issuance digest for an attested [`AuthorityGrant`]: covers every
/// bound axis the grant records (via its serde form, which excludes the
/// private attestation fields) plus the fact's semantic and evidence
/// content digests. Recomputed by `binds()` so post-issuance mutation
/// of either side is detected.
fn grant_content_digest(grant: &AuthorityGrant, fact: &TypedFact) -> String {
    let body = serde_json::json!({
        "grant": serde_json::to_value(grant).unwrap_or(Value::Null),
        "payload_sha256": fact.payload_sha256,
        "evidence_sha256": fact.evidence_sha256,
    });
    sha256_hex(canonical_json(&body).as_bytes())
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

/// Canonical subject identity value used in `fact_id`. Existing v1 axes
/// retain their original explicit-null representation. The additive
/// `interface_identity` key is inserted only when present, so legacy v1 fact
/// identities are byte-for-byte stable while structured facts bind every
/// supplied interface axis.
fn subject_identity_value(subject: &FactSubject) -> Value {
    let mut entity = serde_json::Map::new();
    entity.insert("device_id".into(), json!(&subject.device_id));
    entity.insert("scope".into(), json!(&subject.scope));
    entity.insert("namespace".into(), json!(&subject.namespace));
    entity.insert(
        "network_namespace".into(),
        json!(&subject.network_namespace),
    );
    entity.insert("entity".into(), json!(&subject.entity));
    entity.insert("vrf".into(), json!(&subject.vrf));
    entity.insert("afi_safi".into(), json!(&subject.afi_safi));
    entity.insert("plane".into(), json!(&subject.plane));
    if let Some(interface_identity) = &subject.interface_identity {
        entity.insert("interface_identity".into(), json!(interface_identity));
    }
    Value::Object(entity)
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
        "entity": subject_identity_value(&fact.subject),
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
        PayloadState::NoUsableRecords
            | PayloadState::OutputBudgetExhausted
            | PayloadState::InvalidInput
            | PayloadState::Unsupported
            | PayloadState::UnstructuredFallback
            | PayloadState::UnknownQuality
            | PayloadState::CaptureFailed
    )
}

/// Validate the optional structured interface identity refinement. This
/// function deliberately does not parse vendor display names: producers must
/// supply structured axes only when the source evidence establishes them.
fn validate_interface_subject_identity(fact: &TypedFact) -> Result<(), FactError> {
    let Some(identity) = &fact.subject.interface_identity else {
        return Ok(());
    };
    if fact.subject.namespace != "interface" {
        return Err(FactError::InvalidField(
            "subject.interface_identity outside interface namespace",
        ));
    }
    let entity = fact.subject.entity.as_deref().unwrap_or("");
    if entity.is_empty() {
        return Err(FactError::InvalidField(
            "subject.entity required with interface_identity",
        ));
    }
    fn valid_component(value: &str) -> bool {
        !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| !b.is_ascii_control())
    }
    if !valid_component(entity) {
        return Err(FactError::InvalidField("subject.entity"));
    }
    for (name, value) in [
        ("parent_entity", &identity.parent_entity),
        ("vendor_native_name", &identity.vendor_native_name),
    ] {
        if let Some(value) = value
            && !valid_component(value)
        {
            return Err(FactError::InvalidField(name));
        }
    }
    match identity.kind {
        InterfaceIdentityKind::Subinterface => {
            if identity.subinterface_index.is_none() {
                return Err(FactError::InvalidField(
                    "subinterface_index required for subinterface",
                ));
            }
            let parent = identity
                .parent_entity
                .as_deref()
                .ok_or(FactError::InvalidField(
                    "parent_entity required for subinterface",
                ))?;
            if parent != entity {
                return Err(FactError::InvalidField(
                    "subinterface entity must be canonical parent entity",
                ));
            }
        }
        InterfaceIdentityKind::Logical => {
            if identity.subinterface_index.is_some() {
                return Err(FactError::InvalidField(
                    "logical interface cannot carry subinterface_index",
                ));
            }
        }
        InterfaceIdentityKind::Physical => {
            if identity.subinterface_index.is_some() || identity.parent_entity.is_some() {
                return Err(FactError::InvalidField(
                    "physical base interface cannot carry parent/subinterface axes",
                ));
            }
        }
    }
    Ok(())
}

/// Cross-check the interface subject against the typed interface payload.
///
/// Field relationship contract:
/// - `payload.interface_name` is the exact source-native occurrence name;
/// - `payload.normalized_name`, when present, is the canonical occurrence
///   name selected by the producer;
/// - `subject.entity` is the canonical base/entity key (for a numbered
///   subinterface it is intentionally the parent/base interface);
/// - `interface_identity.vendor_native_name` corroborates
///   `payload.interface_name` and is never parsed as an identity authority.
///
/// Legacy facts without `interface_identity` remain readable even when a
/// historical producer used a native/display name that differs from the
/// canonical entity. New structured facts must not carry arbitrary identity
/// contradictions: their native-name evidence and parent/subinterface axes
/// must agree with the payload relationships when those payload axes are
/// present.
fn validate_interface_subject_payload_consistency(fact: &TypedFact) -> Result<(), FactError> {
    if fact.fact_kind != crate::interface_facts::FACT_KIND_INTERFACE_STATE {
        return Ok(());
    }
    let Some(fields) = &fact.payload.fields else {
        return Ok(());
    };
    let payload: crate::interface_facts::InterfaceStatePayload =
        serde_json::from_value(fields.clone())
            .map_err(|_| FactError::InvalidField("payload.fields.interface.state"))?;
    let Some(identity) = &fact.subject.interface_identity else {
        return Ok(());
    };
    let entity = fact.subject.entity.as_deref().unwrap_or("");
    if let Some(native) = &identity.vendor_native_name
        && *native != payload.interface_name
    {
        return Err(FactError::InvalidField(
            "interface_identity.vendor_native_name/payload.interface_name mismatch",
        ));
    }
    match identity.kind {
        InterfaceIdentityKind::Physical | InterfaceIdentityKind::Logical => {
            // For non-subinterfaces the canonical occurrence and entity key
            // are the same interface. A native spelling difference is valid
            // only when `normalized_name` explicitly carries the canonical
            // form; otherwise the payload could be about an unrelated
            // interface.
            if let Some(normalized) = &payload.normalized_name {
                if normalized != entity {
                    return Err(FactError::InvalidField(
                        "payload.normalized_name/subject.entity mismatch",
                    ));
                }
            } else if payload.interface_name != entity {
                return Err(FactError::InvalidField(
                    "payload.interface_name/subject.entity mismatch",
                ));
            }
        }
        InterfaceIdentityKind::Subinterface => {
            // `subject.entity` is the canonical parent for numbered
            // subinterfaces, so the occurrence name may legitimately differ.
            // If lower-layer parentage is also present in the payload it must
            // corroborate the structured parent rather than contradict it.
            let parent = identity.parent_entity.as_deref().unwrap_or("");
            if let Some(parent_interface) = &payload.parent_interface
                && parent_interface != parent
            {
                return Err(FactError::InvalidField(
                    "payload.parent_interface/interface_identity.parent mismatch",
                ));
            }
            if let Some(parent_rel) = &payload.parent_relationship
                && parent_rel.kind == crate::interface_facts::MasterKind::Parent
                && parent_rel.target != parent
            {
                return Err(FactError::InvalidField(
                    "payload.parent_relationship/interface_identity.parent mismatch",
                ));
            }
        }
    }
    if identity.kind == InterfaceIdentityKind::Logical
        && identity.parent_entity.as_deref() == Some(entity)
    {
        return Err(FactError::InvalidField(
            "logical interface cannot be its own parent",
        ));
    }
    Ok(())
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
        // A serialized grant is evidence only, but it must still bind to
        // THIS fact's current axes — a grant minted for another device,
        // scope, collector or capture is invalid here regardless of
        // attestation.
        if let Some(grant) = &self.authority
            && !grant.binds(self)
        {
            return Err(FactError::InvalidField("authority"));
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
        validate_interface_subject_identity(self)?;
        // Registered fact kinds enforce their typed payload contract.
        if let Some(fields) = &self.payload.fields {
            validate_kind_payload(&self.fact_kind, fields)?;
        }
        validate_interface_subject_payload_consistency(self)?;
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
                interface_identity: None,
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

    /// Synthetic structured-interface helper. These tests are deliberately
    /// structural: neither the current genuine FRR fixture nor the genuine
    /// XRd fixture contains a non-zero subinterface, so no test here claims
    /// vendor evidence that does not exist.
    fn structured_interface_fact(
        namespace: &str,
        kind: InterfaceIdentityKind,
        index: Option<u32>,
        parent: Option<&str>,
        vendor_name: &str,
    ) -> TypedFact {
        let mut f = interface_fact(namespace);
        let canonical_entity = match kind {
            InterfaceIdentityKind::Subinterface => parent.expect("subinterface parent"),
            _ => "Ethernet1",
        };
        f.subject.entity = Some(canonical_entity.into());
        f.subject.interface_identity = Some(InterfaceSubjectIdentity {
            kind,
            parent_entity: parent.map(str::to_string),
            subinterface_index: index,
            vendor_native_name: Some(vendor_name.into()),
        });
        if let Some(fields) = f.payload.fields.as_mut()
            && let Some(obj) = fields.as_object_mut()
        {
            obj.insert("interface_name".into(), vendor_name.into());
        }
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
    fn structured_interface_identity_is_additive_and_collision_safe() {
        // Existing unstructured v1 identity remains valid and stable.
        let legacy = interface_fact("asic-0");
        legacy.validate().unwrap();
        assert!(legacy.subject.interface_identity.is_none());

        let physical = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Physical,
            None,
            None,
            "Ethernet1",
        );
        let subzero = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(0),
            Some("Ethernet1"),
            "Ethernet1.0",
        );
        let sub100 = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        let sub200 = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(200),
            Some("Ethernet1"),
            "Ethernet1.200",
        );
        for fact in [&physical, &subzero, &sub100, &sub200] {
            fact.validate().unwrap();
        }
        assert_ne!(physical.fact_id, subzero.fact_id);
        assert_ne!(subzero.fact_id, sub100.fact_id);
        assert_ne!(sub100.fact_id, sub200.fact_id);

        // Same vendor-native interface identity in another ASIC namespace
        // cannot collide even when every structured interface axis matches.
        let other_asic = structured_interface_fact(
            "asic-1",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        assert_ne!(sub100.fact_id, other_asic.fact_id);
    }

    #[test]
    fn structured_interface_identity_validation_rejects_inconsistent_axes() {
        let mut missing_index = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        missing_index
            .subject
            .interface_identity
            .as_mut()
            .unwrap()
            .subinterface_index = None;
        missing_index.seal();
        assert!(missing_index.validate().is_err());

        let mut missing_parent = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        missing_parent
            .subject
            .interface_identity
            .as_mut()
            .unwrap()
            .parent_entity = None;
        missing_parent.seal();
        assert!(missing_parent.validate().is_err());

        let mut wrong_entity = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        wrong_entity.subject.entity = Some("Ethernet1.100".into());
        wrong_entity.seal();
        assert!(wrong_entity.validate().is_err());

        let mut logical_with_index = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Logical,
            None,
            None,
            "Vlan100",
        );
        logical_with_index
            .subject
            .interface_identity
            .as_mut()
            .unwrap()
            .subinterface_index = Some(100);
        logical_with_index.seal();
        assert!(logical_with_index.validate().is_err());

        let mut physical_with_parent = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Physical,
            None,
            None,
            "Ethernet1",
        );
        physical_with_parent
            .subject
            .interface_identity
            .as_mut()
            .unwrap()
            .parent_entity = Some("Ethernet0".into());
        physical_with_parent.seal();
        assert!(physical_with_parent.validate().is_err());

        let mut wrong_namespace = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Physical,
            None,
            None,
            "Ethernet1",
        );
        wrong_namespace.subject.namespace = "routing".into();
        wrong_namespace.seal();
        assert!(wrong_namespace.validate().is_err());
    }

    #[test]
    fn structured_interface_identity_is_bound_into_authority() {
        let res = resolver_resolution("edge-rtr-01");
        let mut fact = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        fact.trust = TrustClass::Corroborated;
        fact.authority = AuthorityGrant::for_fact(&res, &fact);
        assert!(fact.authority_eligible());
        fact.seal();
        fact.validate().unwrap();

        // Any structured-axis mutation after issuance invalidates the grant,
        // including vendor-native evidence and the numeric subinterface axis.
        for mutate in [
            (|f: &mut TypedFact| {
                f.subject
                    .interface_identity
                    .as_mut()
                    .unwrap()
                    .subinterface_index = Some(200);
            }) as fn(&mut TypedFact),
            (|f: &mut TypedFact| {
                f.subject
                    .interface_identity
                    .as_mut()
                    .unwrap()
                    .vendor_native_name = Some("Ethernet1.200".into());
            }),
        ] {
            let res = resolver_resolution("edge-rtr-01");
            let mut f = structured_interface_fact(
                "asic-0",
                InterfaceIdentityKind::Subinterface,
                Some(100),
                Some("Ethernet1"),
                "Ethernet1.100",
            );
            f.trust = TrustClass::Corroborated;
            f.authority = AuthorityGrant::for_fact(&res, &f);
            assert!(f.authority_eligible());
            mutate(&mut f);
            f.seal();
            assert!(matches!(
                f.validate(),
                Err(FactError::InvalidField("authority"))
            ));
            assert!(!f.authority_eligible());
        }
    }

    #[test]
    fn serialized_structured_identity_roundtrips_but_authority_does_not_reattest() {
        let res = resolver_resolution("edge-rtr-01");
        let mut fact = structured_interface_fact(
            "asic-0",
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        fact.trust = TrustClass::Corroborated;
        fact.authority = AuthorityGrant::for_fact(&res, &fact);
        assert!(fact.authority_eligible());
        fact.seal();
        let bytes = fact.to_vec().unwrap();
        let back = TypedFact::from_slice(&bytes).unwrap();
        assert_eq!(back.fact_id, fact.fact_id);
        assert_eq!(
            back.subject.interface_identity,
            fact.subject.interface_identity
        );
        assert!(back.authority.as_ref().is_some_and(|g| !g.is_attested()));
        assert!(!back.authority_eligible());
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

    /// One sealed fact per `PayloadState` variant — the ABI drift gate
    /// artifact. Regenerate with:
    ///   cargo test -p bonsai --lib typed_facts -- --ignored emit_state_fixtures
    #[test]
    #[ignore = "artifact generator; run explicitly to regenerate state fixtures"]
    fn emit_state_fixtures() {
        let states = [
            PayloadState::Empty,
            PayloadState::InvalidInput,
            PayloadState::Unsupported,
            PayloadState::UnstructuredFallback,
            PayloadState::UnknownQuality,
            PayloadState::CaptureFailed,
            PayloadState::NoUsableRecords,
            PayloadState::OutputBudgetExhausted,
        ];
        let facts: Vec<Value> = states
            .iter()
            .map(|state| {
                let mut f = sample_fact();
                f.payload.fields = None;
                f.payload.state = Some(*state);
                // Degraded states cannot claim complete coverage.
                f.quality.completeness = Completeness::Unknown;
                f.seal();
                serde_json::to_value(&f).unwrap()
            })
            .collect();
        let doc = serde_json::json!({
            "description": "One Rust-emitted TypedFact per PayloadState variant (BN-024-02 ABI drift gate). Each entry is a complete serialized v1 fact.",
            "facts": facts,
        });
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/bn024/typed-facts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("v1.states.fixture.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
    }

    /// Every committed state fixture must deserialize, validate, and
    /// cover EXACTLY the `PayloadState` variant set — a variant missing
    /// from Rust, the fixture or (via the python schema gate) the JSON
    /// schema is drift and fails here.
    #[test]
    fn committed_state_fixtures_cover_every_payload_state() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/bn024/typed-facts/v1.states.fixture.json");
        let bytes = std::fs::read(&path).expect("state fixtures artifact missing");
        let doc: Value = serde_json::from_slice(&bytes).expect("state fixtures not JSON");
        let facts = doc["facts"].as_array().expect("facts array");
        let mut seen: Vec<PayloadState> = Vec::new();
        for entry in facts {
            let fact = TypedFact::from_slice(
                &serde_json::to_vec(entry).expect("state fixture entry serializable"),
            )
            .expect("state fixture must validate under v1 rules");
            assert!(fact.payload.fields.is_none());
            let state = fact.payload.state.expect("state fixture carries a state");
            assert!(
                !seen.contains(&state),
                "duplicate state fixture for {state:?}"
            );
            seen.push(state);
        }
        let expected = [
            PayloadState::Empty,
            PayloadState::InvalidInput,
            PayloadState::Unsupported,
            PayloadState::UnstructuredFallback,
            PayloadState::UnknownQuality,
            PayloadState::CaptureFailed,
            PayloadState::NoUsableRecords,
            PayloadState::OutputBudgetExhausted,
        ];
        assert_eq!(seen.len(), expected.len());
        for state in expected {
            assert!(seen.contains(&state), "missing state fixture for {state:?}");
        }
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

    // ── B2: attestation must bind the contents it attests ─────────────

    /// A resolver-attested resolution whose PUBLIC evidence fields are
    /// mutated after resolution must not mint a grant — the private
    /// attestation flag alone cannot cover mutable contents.
    #[test]
    fn mutated_resolution_cannot_mint_grant() {
        // Mutate the canonical device id to another device.
        let mut res = resolver_resolution("edge-rtr-01");
        assert!(res.is_resolver_attested());
        res.device_id = Some("edge-rtr-02".into());
        let mut fact_b = sample_fact();
        fact_b.subject.device_id = "edge-rtr-02".into();
        fact_b.seal();
        assert!(AuthorityGrant::for_fact(&res, &fact_b).is_none());

        // Mutate observed scope/collector/session/exporter axes so they
        // corroborate a DIFFERENT fact's source.
        let mut res = resolver_resolution("edge-rtr-01");
        res.observed.collector_id = Some("collector-b".into());
        res.observed.session_epoch = Some(9);
        res.observed.scope = Some("tenant-x".into());
        let mut fact = sample_fact();
        fact.source.collector_id = "collector-b".into();
        fact.source.session_epoch = 9;
        fact.subject.scope = Some("tenant-x".into());
        fact.seal();
        assert!(AuthorityGrant::for_fact(&res, &fact).is_none());

        // Mutate the resolution's canonical exporter id.
        let mut res = resolver_resolution("edge-rtr-01");
        res.canonical_exporter_id = Some("exp-other".into());
        let mut fact = sample_fact();
        fact.source.exporter_id = Some("exp-other".into());
        fact.seal();
        assert!(AuthorityGrant::for_fact(&res, &fact).is_none());

        // Mutating an axis the grant path does not consult (observed
        // transport address) still breaks the attestation.
        let mut res = resolver_resolution("edge-rtr-01");
        res.observed.transport_source = "10.9.9.9".into();
        assert!(AuthorityGrant::for_fact(&res, &sample_fact()).is_none());
    }

    /// A cloned legitimate grant has public bound axes: mutating them
    /// TOGETHER with the fact's matching axes and resealing must still
    /// fail — the issuance digest covers both sides.
    #[test]
    fn consistent_grant_and_fact_mutation_still_fails() {
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.trust = TrustClass::Corroborated;
        f.authority = AuthorityGrant::for_fact(&res, &f);
        f.seal();
        assert!(f.authority_eligible());
        let grant = f.authority.clone().unwrap();

        // Move BOTH the grant's bound axes and the fact's axes to a
        // different collector/capture consistently, then reseal.
        let mut moved = sample_fact();
        moved.trust = TrustClass::Corroborated;
        moved.source.collector_id = "collector-b".into();
        moved.source.capture_id = "cap-moved".into();
        moved.seal();
        let mut g = grant.clone();
        g.collector_id = "collector-b".into();
        g.capture_id = "cap-moved".into();
        moved.authority = Some(g);
        moved.seal();
        assert!(
            matches!(moved.validate(), Err(FactError::InvalidField("authority"))),
            "mutated attested grant must fail validation"
        );
        assert!(!moved.authority_eligible());
    }

    /// Mutating the fact's semantic payload or evidence while keeping
    /// the grant must break eligibility — the grant binds the fact's
    /// payload/evidence digests, not just its identity axes.
    #[test]
    fn payload_or_evidence_mutation_breaks_grant() {
        let res = resolver_resolution("edge-rtr-01");
        for mutate in [
            (|f: &mut TypedFact| {
                f.payload.fields = Some(serde_json::json!({"neighbors": 99}));
            }) as fn(&mut TypedFact),
            (|f: &mut TypedFact| {
                f.evidence = vec![EvidenceRef {
                    kind: "raw_output".into(),
                    sha256: "b".repeat(64),
                    locator: None,
                }];
            }) as fn(&mut TypedFact),
        ] {
            let mut f = sample_fact();
            f.trust = TrustClass::Corroborated;
            f.authority = AuthorityGrant::for_fact(&res, &f);
            f.seal();
            assert!(f.authority_eligible());
            mutate(&mut f);
            f.seal(); // reseal so digests are self-consistent
            assert!(
                matches!(f.validate(), Err(FactError::InvalidField("authority"))),
                "grant must not survive payload/evidence mutation"
            );
            assert!(!f.authority_eligible());
        }
    }

    /// Authority cannot be derived from replayed/deserialized state:
    /// a serialized grant loses attestation AND a mutated-then-
    /// serialized resolution cannot exist (IdentityResolution is not
    /// deserializable) — pin both boundaries.
    #[test]
    fn replayed_state_confers_no_authority() {
        // Grant round-trip: axes survive, attestation does not.
        let res = resolver_resolution("edge-rtr-01");
        let mut f = sample_fact();
        f.trust = TrustClass::Corroborated;
        f.authority = AuthorityGrant::for_fact(&res, &f);
        f.seal();
        let back = TypedFact::from_slice(&f.to_vec().unwrap()).unwrap();
        assert!(back.authority.is_some());
        assert!(!back.authority_eligible());

        // A resolution produced for capture/session A cannot mint for
        // an unrelated capture/session B even when device+scope match.
        let binding = TrustedBinding {
            binding_id: "bind-replay".into(),
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
        let observed_a = TelemetrySourceIdentity::observed("10.0.0.1")
            .with_protocol("cli_ssh")
            .with_collector("collector-a")
            .with_session_epoch(3);
        let res_a = IdentityResolver::resolve(&observed_a, std::slice::from_ref(&binding));
        let mut fact_b = sample_fact();
        fact_b.source.capture_id = "cap-unrelated".into();
        fact_b.source.session_epoch = 77;
        fact_b.seal();
        // for_fact corroborates collector+epoch against the observed
        // envelope; capture_id/sequence are bind-time axes only, so the
        // grant is issuable — but it binds THIS fact's axes, and any
        // later reuse for another capture breaks binds().
        assert!(AuthorityGrant::for_fact(&res_a, &fact_b).is_none());
    }
}

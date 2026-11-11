//! BN-021b: canonical telemetry source identity contract.
//!
//! Three distinct identities, never conflated:
//! - **transport endpoint**: the observed socket address that delivered
//!   the telemetry (may be NAT'd, relayed, or ephemeral).
//! - **exporter identity**: the protocol-level exporting context
//!   (BMP speaker, NetFlow source-id / IPFIX observation-domain,
//!   syslog relay, SNMP engine, sFlow agent, OTLP resource).
//! - **device identity**: the canonical inventory entity the
//!   observation belongs to (resolved via trusted bindings only).
//!
//! Rules:
//! - `device_id` is set ONLY by a trusted inventory binding — never
//!   inferred from transport IP or self-reported names alone.
//! - Ambiguous/conflicting identities must not mutate authoritative
//!   device state; callers should quarantine or mark limited.
//! - Transport provenance is always preserved alongside resolved
//!   identity so historical records remain auditable.
//! - `bmp:{sysName}` and IP-derived exporter claims are PROVISIONAL
//!   evidence. They may scope a trusted binding lookup but never
//!   constitute a trusted identity by themselves.

use serde::{Deserialize, Serialize};

/// Confidence/trust state of a source-identity resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    /// Unique trusted device/exporter match established.
    Resolved,
    /// Candidate identity exists but is not sufficiently verified.
    Provisional,
    /// Multiple possible identities — must not merge into device state.
    Ambiguous,
    /// No matching device/exporter known.
    Unknown,
    /// Transport and protocol identity contradict a trusted binding.
    Conflicting,
}

impl IdentityStatus {
    /// Whether observations with this status may update authoritative
    /// device state. Only `Resolved` may; ambiguous/unknown/conflicting
    /// are quarantined by callers.
    pub fn may_update_authoritative_state(self) -> bool {
        matches!(self, Self::Resolved)
    }
}

/// Scope applied when an observation carries no explicit scope. Bindings
/// are always scoped; an unscoped observation can therefore only ever
/// match bindings in the `"default"` scope — never a site/tenant scope.
pub const DEFAULT_IDENTITY_SCOPE: &str = "default";

/// How a [`VerifiedIngressPrincipal`] was established. Only transport
/// mechanisms that cryptographically authenticate the peer may appear
/// here — self-reported protocol fields are never a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressAuthMethod {
    /// Mutual TLS: the client presented a certificate chaining to a
    /// configured trusted CA and the handshake completed.
    MutualTls,
}

/// A transport-authenticated exporter principal (BN-021f).
///
/// This value is constructed ONLY by the ingress authentication layer
/// after a successful transport handshake — never from BMP payload
/// fields, headers, PROXY metadata, or any other client-supplied data.
/// It is the sole input allowed to promote an observation to canonical
/// device attribution.
///
/// Trust boundary (BN-021f hardening): fields are private and the only
/// constructor is crate-internal, so ordinary application code cannot
/// mint a principal. `transport_attested` is NEVER serialized — a
/// principal restored from JSON/journal data deserializes with
/// `transport_attested = false` and is rejected by
/// [`IdentityResolver::resolve_verified_ingress`]. Journals may carry
/// the credential fingerprint as provenance evidence, but replayed or
/// API-supplied data can never re-authorize a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedIngressPrincipal {
    method: IngressAuthMethod,
    /// Cryptographic credential identifier: `"sha256:<hex>"` of the
    /// DER-encoded client certificate leaf presented during the mTLS
    /// handshake. Exact fingerprint matching makes credential rotation
    /// an explicit binding update, never an implicit trust change.
    credential_fingerprint: String,
    /// Set only by [`Self::from_transport_attestation`]; skipped by
    /// serde so deserialized values are always unattested.
    #[serde(skip)]
    transport_attested: bool,
}

impl VerifiedIngressPrincipal {
    /// Construct a principal from a completed transport authentication
    /// handshake. Crate-internal: only the authenticating ingress layer
    /// (`routing_service`) may call this.
    pub(crate) fn from_transport_attestation(
        method: IngressAuthMethod,
        credential_fingerprint: String,
    ) -> Self {
        Self {
            method,
            credential_fingerprint,
            transport_attested: true,
        }
    }

    /// The transport authentication mechanism that established this
    /// principal.
    pub fn method(&self) -> IngressAuthMethod {
        self.method
    }

    /// The verified credential identifier (`"sha256:<hex>"`).
    pub fn credential_fingerprint(&self) -> &str {
        &self.credential_fingerprint
    }

    /// Whether this principal was produced by a live transport
    /// authentication handshake in this process. Always `false` for
    /// values deserialized from journals/API payloads.
    pub fn is_transport_attested(&self) -> bool {
        self.transport_attested
    }
}

impl<'de> Deserialize<'de> for VerifiedIngressPrincipal {
    /// Deserialization preserves the credential evidence but NEVER the
    /// attestation — a principal loaded from stored data cannot
    /// authorize verified-ingress resolution.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Repr {
            method: IngressAuthMethod,
            credential_fingerprint: String,
        }
        let repr = Repr::deserialize(deserializer)?;
        Ok(Self {
            method: repr.method,
            credential_fingerprint: repr.credential_fingerprint,
            transport_attested: false,
        })
    }
}

/// Protocol-agnostic identity envelope carried by ingestion adapters.
/// All fields are evidence, not conclusions: `exporter_id` and
/// `device_id` are `None` until a trusted binding resolves them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySourceIdentity {
    /// Observed transport endpoint as delivered to the collector
    /// (e.g. "10.1.1.10" or "10.1.1.10:49152"). Never treated as device
    /// identity by itself.
    pub transport_source: String,
    /// Protocol-level exporter context, e.g.
    /// "bmp:172.100.207.12", "nf9:10.1.1.10#sid=0",
    /// "ipfix:10.1.1.10#dom=42", "syslog-relay:10.1.1.50".
    /// PROVISIONAL: derived from observed/protocol-reported data.
    pub exporter_id: Option<String>,
    /// Canonical inventory device id — only set via trusted binding.
    pub device_id: Option<String>,
    /// Self-reported identity claim (hostname/sysName/engineID/resource
    /// attr). Evidence only; requires validation before use.
    pub reported_identity: Option<String>,
    /// Observation domain / VRF / tenant scope when the protocol exposes
    /// one (IPFIX ODID, NetFlow source-id, BMP RD distinguisher).
    pub observation_domain_id: Option<u32>,
    /// Protocol that produced the observation ("bmp", "netflow9",
    /// "ipfix", "syslog", "snmp", "sflow", "otlp", "gnmi", "bgp-ls").
    /// Required for trusted resolution; `None` can never resolve.
    pub protocol: Option<String>,
    /// Tenant/site/network-domain scope the observation was collected
    /// under. `None` resolves against [`DEFAULT_IDENTITY_SCOPE`] only.
    pub scope: Option<String>,
    /// Collector that received the telemetry, when known.
    pub collector_id: Option<String>,
    /// Connection/session generation fencing stale reconnects
    /// (e.g. BMP `session_epoch`).
    pub session_epoch: Option<u64>,
    /// Resolution confidence.
    pub identity_status: IdentityStatus,
    /// Transport-authenticated principal established by the ingress
    /// layer (BN-021f). `None` for unauthenticated/plaintext transports
    /// and for envelopes that never crossed an authenticating listener.
    /// Never populated from protocol-reported fields.
    #[serde(default)]
    pub verified_ingress: Option<VerifiedIngressPrincipal>,
}

impl TelemetrySourceIdentity {
    /// Minimal envelope for a source with no resolved identity yet.
    pub fn observed(transport_source: impl Into<String>) -> Self {
        Self {
            transport_source: transport_source.into(),
            exporter_id: None,
            device_id: None,
            reported_identity: None,
            observation_domain_id: None,
            protocol: None,
            scope: None,
            collector_id: None,
            session_epoch: None,
            identity_status: IdentityStatus::Unknown,
            verified_ingress: None,
        }
    }

    /// Attach a protocol-level exporter context (still unverified).
    pub fn with_exporter(mut self, exporter_id: impl Into<String>) -> Self {
        self.exporter_id = Some(exporter_id.into());
        self
    }

    /// Attach a self-reported identity claim (still unverified).
    pub fn with_reported(mut self, reported: impl Into<String>) -> Self {
        self.reported_identity = Some(reported.into());
        self
    }

    /// Attach an observation-domain scope.
    pub fn with_domain(mut self, domain: u32) -> Self {
        self.observation_domain_id = Some(domain);
        self
    }

    /// Attach the producing protocol.
    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = Some(protocol.into());
        self
    }

    /// Attach the tenant/site/network-domain collection scope.
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// Attach the receiving collector identity.
    pub fn with_collector(mut self, collector_id: impl Into<String>) -> Self {
        self.collector_id = Some(collector_id.into());
        self
    }

    /// Attach a connection/session generation.
    pub fn with_session_epoch(mut self, epoch: u64) -> Self {
        self.session_epoch = Some(epoch);
        self
    }

    /// Attach a transport-verified ingress principal. Only the
    /// authenticating transport layer may call this — the value is
    /// never derived from observation content.
    pub fn with_verified_ingress(mut self, principal: VerifiedIngressPrincipal) -> Self {
        self.verified_ingress = Some(principal);
        self
    }

    /// The scope this observation resolves under.
    pub fn resolution_scope(&self) -> &str {
        self.scope.as_deref().unwrap_or(DEFAULT_IDENTITY_SCOPE)
    }
}

// ─── Trusted inventory bindings (BN-021b) ────────────────────────────────────

/// Where a trusted binding came from. Bindings may ONLY be created by
/// trusted provisioning paths (operator action, inventory import,
/// migration) — never by telemetry content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingProvenance {
    /// Explicit operator action through an authorized admin path.
    OperatorProvisioned,
    /// Imported from trusted inventory/onboarding data.
    InventoryImport,
    /// Carried forward by a schema/data migration.
    Migration,
}

/// Lifecycle state of a trusted binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    Active,
    /// Explicitly invalidated; no longer claims observations.
    Revoked,
}

/// A trusted association between an observed exporter context and a
/// canonical inventory device. Created only via provisioning APIs —
/// telemetry can never mint or modify one.
///
/// Match fields are OPTIONAL exact-equality constraints on observed
/// evidence; every `Some` field must equal the observation for the
/// binding to claim it. A binding with no match fields claims every
/// observation in its (scope, protocol) — useful for single-exporter
/// scopes, dangerous otherwise (registration overlap checks reject
/// ambiguous provisioning).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustedBinding {
    /// Stable binding identifier (assigned at creation).
    pub binding_id: String,
    /// Tenant/site/network-domain scope. Must equal the observation's
    /// resolution scope for the binding to apply.
    pub scope: String,
    /// Protocol this binding applies to ("bmp", "netflow9", ...).
    pub protocol: String,
    /// Canonical logical exporter identifier assigned at provisioning
    /// (e.g. "exp-site1-rtr-a"). Unique per (scope, protocol).
    pub exporter_id: String,
    /// Canonical inventory device id this exporter belongs to.
    pub device_id: String,
    /// Optional exact-match constraints on observed evidence.
    #[serde(default)]
    pub match_transport: Option<String>,
    #[serde(default)]
    pub match_reported_identity: Option<String>,
    #[serde(default)]
    pub match_exporter_claim: Option<String>,
    #[serde(default)]
    pub match_observation_domain: Option<u32>,
    /// Optional exact-match constraint on the VERIFIED transport
    /// credential (`"sha256:<hex>"` client-certificate fingerprint,
    /// BN-021f). When set, the binding can only claim observations that
    /// arrived over a transport-authenticated connection presenting
    /// this exact credential — self-reported BMP fields can never
    /// satisfy it. A binding WITHOUT this pin can never authorize a
    /// verified-ingress resolution (see
    /// [`IdentityResolver::resolve_verified_ingress`]).
    #[serde(default)]
    pub match_ingress_credential: Option<String>,
    /// Trusted origin of this binding.
    pub provenance: BindingProvenance,
    pub status: BindingStatus,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
    pub created_by: String,
    pub updated_by: String,
}

impl TrustedBinding {
    /// Whether this binding is allowed to claim observations.
    pub fn is_active(&self) -> bool {
        self.status == BindingStatus::Active
    }
}

/// A canonical inventory device — the persistent entity telemetry
/// resolves to. Keyed by an opaque `device_id` that survives management
/// address changes; `address` is the CURRENT observed/inventory address
/// and may be updated without changing `device_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalDevice {
    /// Stable canonical identifier (e.g. "dev-<uuid>"). Assigned at
    /// registration; never derived from IP or hostname.
    pub device_id: String,
    /// Current inventory/management address. Evidence, not identity.
    pub address: String,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
    pub created_by: String,
    pub updated_by: String,
}

/// Deterministic outcome of resolving an observed identity against a
/// set of trusted bindings. `observed` always carries the raw evidence;
/// canonical ids are populated only for `Resolved`.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityResolution {
    pub status: IdentityStatus,
    /// Canonical inventory device id — `Some` only when Resolved.
    pub device_id: Option<String>,
    /// Canonical logical exporter id — `Some` only when Resolved.
    pub canonical_exporter_id: Option<String>,
    /// Trusted binding that produced the resolution.
    pub binding_id: Option<String>,
    /// Provenance of that binding.
    pub provenance: Option<BindingProvenance>,
    /// The original observed envelope, evidence preserved verbatim.
    pub observed: TelemetrySourceIdentity,
}

impl IdentityResolution {
    /// Whether this resolution may drive authoritative device state.
    pub fn may_update_authoritative_state(&self) -> bool {
        self.status.may_update_authoritative_state()
    }
}

/// Stateless deterministic resolver. Bindings are supplied by the
/// caller (typically loaded from the persistent store); the resolver
/// performs no I/O and has no hidden state, so results depend only on
/// the observed envelope and the binding set.
pub struct IdentityResolver;

impl IdentityResolver {
    /// Resolve `observed` against `bindings`.
    ///
    /// Semantics:
    /// - **Resolved**: exactly one distinct (device_id, exporter_id)
    ///   pair is claimed by active bindings.
    /// - **Ambiguous**: multiple distinct pairs are claimed — no
    ///   arbitrary first-match selection.
    /// - **Conflicting**: no binding fully claims the observation, but
    ///   an active binding in the same scope+protocol partially matches
    ///   while contradicting on another pinned field.
    /// - **Provisional**: the observation carries protocol/reported
    ///   identity evidence but no trusted binding applies.
    /// - **Unknown**: no usable identity evidence at all.
    pub fn resolve(
        observed: &TelemetrySourceIdentity,
        bindings: &[TrustedBinding],
    ) -> IdentityResolution {
        Self::resolve_against(
            observed,
            bindings.iter().filter(|b| {
                // Credential-pinned bindings can only be claimed through the
                // verified-ingress path; the claim-based resolver must never
                // satisfy them from self-reported evidence alone.
                b.match_ingress_credential.is_none()
            }),
        )
    }

    /// Resolve `observed` for a transport-authenticated connection
    /// (BN-021f).
    ///
    /// Authorization rule: a binding may claim the observation ONLY when
    /// it pins `match_ingress_credential` to the exact verified
    /// credential fingerprint presented by the transport. Certificate
    /// validity alone is never sufficient — the credential must map to
    /// an explicitly provisioned binding. Bindings without a credential
    /// pin are invisible to this path.
    ///
    /// Once the credential matches, the remaining pinned evidence
    /// dimensions (transport, reported identity, exporter claim,
    /// observation domain) are evaluated exactly as in [`Self::resolve`]:
    /// contradictions fail closed, multiple distinct canonical pairs are
    /// ambiguous, and exactly one claimant resolves.
    pub fn resolve_verified_ingress(
        observed: &TelemetrySourceIdentity,
        bindings: &[TrustedBinding],
        principal: &VerifiedIngressPrincipal,
    ) -> IdentityResolution {
        // Fail closed on unattested principals: values deserialized
        // from journals, API payloads, or any non-transport source can
        // never authorize canonical attribution.
        if !principal.is_transport_attested() {
            return Self::resolve_against(observed, std::iter::empty());
        }
        let fp = principal.credential_fingerprint();
        Self::resolve_against(
            observed,
            bindings
                .iter()
                .filter(|b| b.match_ingress_credential.as_deref() == Some(fp)),
        )
    }

    fn resolve_against<'a>(
        observed: &TelemetrySourceIdentity,
        bindings: impl Iterator<Item = &'a TrustedBinding>,
    ) -> IdentityResolution {
        let scope = observed.resolution_scope();
        let protocol = observed.protocol.as_deref();

        let mut claimants: Vec<&TrustedBinding> = Vec::new();
        let mut contradictions = false;

        for binding in bindings.filter(|b| b.is_active()) {
            // Scope and protocol must match before any evidence check.
            if binding.scope != scope || Some(binding.protocol.as_str()) != protocol {
                continue;
            }
            if binding_claims(binding, observed) {
                claimants.push(binding);
                continue;
            }
            if binding_contradicts(binding, observed) {
                contradictions = true;
            }
        }

        // Collapse claimants that agree on the canonical pair.
        claimants
            .sort_by(|a, b| (&a.device_id, &a.exporter_id).cmp(&(&b.device_id, &b.exporter_id)));
        claimants.dedup_by(|a, b| a.device_id == b.device_id && a.exporter_id == b.exporter_id);

        let (status, device_id, canonical_exporter_id, binding_id, provenance) =
            match claimants.as_slice() {
                [single] => (
                    IdentityStatus::Resolved,
                    Some(single.device_id.clone()),
                    Some(single.exporter_id.clone()),
                    Some(single.binding_id.clone()),
                    Some(single.provenance),
                ),
                [] if contradictions => (IdentityStatus::Conflicting, None, None, None, None),
                [] => {
                    let has_evidence =
                        observed.exporter_id.is_some() || observed.reported_identity.is_some();
                    (
                        if has_evidence {
                            IdentityStatus::Provisional
                        } else {
                            IdentityStatus::Unknown
                        },
                        None,
                        None,
                        None,
                        None,
                    )
                }
                _ => (IdentityStatus::Ambiguous, None, None, None, None),
            };

        IdentityResolution {
            status,
            device_id,
            canonical_exporter_id,
            binding_id,
            provenance,
            observed: observed.clone(),
        }
    }
}

/// Per-dimension comparison of one pinned binding field against the
/// observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldMatch {
    /// Binding does not constrain this dimension.
    Unconstrained,
    /// Binding pins this dimension and it equals the observation.
    Matches,
    /// Binding pins this dimension and it differs from the observation.
    Contradicts,
}

fn compare_opt<T: PartialEq>(pinned: Option<&T>, observed: Option<&T>) -> FieldMatch {
    match pinned {
        None => FieldMatch::Unconstrained,
        Some(p) if Some(p) == observed => FieldMatch::Matches,
        Some(_) => FieldMatch::Contradicts,
    }
}

fn field_matches(binding: &TrustedBinding, observed: &TelemetrySourceIdentity) -> [FieldMatch; 4] {
    [
        compare_opt(
            binding.match_transport.as_ref(),
            Some(&observed.transport_source),
        ),
        compare_opt(
            binding.match_reported_identity.as_ref(),
            observed.reported_identity.as_ref(),
        ),
        compare_opt(
            binding.match_exporter_claim.as_ref(),
            observed.exporter_id.as_ref(),
        ),
        compare_opt(
            binding.match_observation_domain.as_ref(),
            observed.observation_domain_id.as_ref(),
        ),
    ]
}

/// A binding claims an observation when scope+protocol match and every
/// pinned field equals the observed value.
fn binding_claims(binding: &TrustedBinding, observed: &TelemetrySourceIdentity) -> bool {
    binding.scope == observed.resolution_scope()
        && Some(binding.protocol.as_str()) == observed.protocol.as_deref()
        && field_matches(binding, observed)
            .iter()
            .all(|m| *m != FieldMatch::Contradicts)
}

/// A binding contradicts an observation in the same scope+protocol when
/// at least one pinned field matches and at least one other pinned
/// field contradicts — i.e. the evidence points at this binding but
/// cannot be it.
fn binding_contradicts(binding: &TrustedBinding, observed: &TelemetrySourceIdentity) -> bool {
    if binding.scope != observed.resolution_scope()
        || Some(binding.protocol.as_str()) != observed.protocol.as_deref()
    {
        return false;
    }
    let fields = field_matches(binding, observed);
    fields.contains(&FieldMatch::Matches) && fields.contains(&FieldMatch::Contradicts)
}

/// Whether two active bindings could both claim the same observation —
/// i.e. they do not pin any dimension to different values. Used at
/// provisioning time to reject ambiguous registrations.
pub fn bindings_overlap(a: &TrustedBinding, b: &TrustedBinding) -> bool {
    if a.scope != b.scope || a.protocol != b.protocol {
        return false;
    }
    let str_dims = [
        (a.match_transport.as_ref(), b.match_transport.as_ref()),
        (
            a.match_reported_identity.as_ref(),
            b.match_reported_identity.as_ref(),
        ),
        (
            a.match_exporter_claim.as_ref(),
            b.match_exporter_claim.as_ref(),
        ),
        (
            a.match_ingress_credential.as_ref(),
            b.match_ingress_credential.as_ref(),
        ),
    ];
    str_dims
        .iter()
        .all(|(x, y)| x.is_none() || y.is_none() || x == y)
        && (a.match_observation_domain.is_none()
            || b.match_observation_domain.is_none()
            || a.match_observation_domain == b.match_observation_domain)
}

// ─── Protocol adapters ───────────────────────────────────────────────────────

/// Narrow adapter: build the common identity envelope from a BMP
/// `ObservationSource` plus Initiation-reported metadata.
///
/// `source.exporter_id` is the PROVISIONAL claim already computed by
/// the RIB layer (e.g. "bmp:{sysName}" or the transport address) — it
/// is preserved as `exporter_id` evidence, never trusted. `sys_name`
/// is preserved separately as `reported_identity`.
///
/// This adapter does NOT make BMP ingestion NAT-safe; it only produces
/// the envelope a trusted resolver can evaluate.
pub fn identity_from_bmp_observation(
    source: &rbmp_core::bmp::types::ObservationSource,
    sys_name: Option<&str>,
    scope: Option<String>,
) -> TelemetrySourceIdentity {
    let mut identity = TelemetrySourceIdentity::observed(source.exporter_addr.to_string())
        .with_protocol("bmp")
        .with_exporter(source.exporter_id.clone())
        .with_collector(source.collector_id.clone())
        .with_session_epoch(source.session_epoch);
    if let Some(name) = sys_name.map(str::trim).filter(|s| !s.is_empty()) {
        identity = identity.with_reported(name);
    }
    if let Some(scope) = scope {
        identity = identity.with_scope(scope);
    }
    identity
}

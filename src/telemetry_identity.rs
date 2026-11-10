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

use serde::Serialize;

/// Confidence/trust state of a source-identity resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    /// device state. Only `Resolved` (and narrowly `Provisional` for
    /// non-authoritative views) may; ambiguous/unknown/conflicting are
    /// quarantined by callers.
    pub fn may_update_authoritative_state(self) -> bool {
        matches!(self, Self::Resolved)
    }
}

/// Protocol-agnostic identity envelope carried by ingestion adapters.
/// All fields are evidence, not conclusions: `exporter_id` and
/// `device_id` are `None` until a trusted binding resolves them.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySourceIdentity {
    /// Observed transport endpoint as delivered to the collector
    /// (e.g. "10.1.1.10" or "10.1.1.10:49152"). Never treated as device
    /// identity by itself.
    pub transport_source: String,
    /// Protocol-level exporter context, e.g.
    /// "bmp:172.100.207.12", "nf9:10.1.1.10#sid=0",
    /// "ipfix:10.1.1.10#dom=42", "syslog-relay:10.1.1.50".
    pub exporter_id: Option<String>,
    /// Canonical inventory device id — only set via trusted binding.
    pub device_id: Option<String>,
    /// Self-reported identity claim (hostname/sysName/engineID/resource
    /// attr). Evidence only; requires validation before use.
    pub reported_identity: Option<String>,
    /// Observation domain / VRF / tenant scope when the protocol exposes
    /// one (IPFIX ODID, NetFlow source-id, BMP RD distinguisher).
    pub observation_domain_id: Option<u32>,
    /// Resolution confidence.
    pub identity_status: IdentityStatus,
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
            identity_status: IdentityStatus::Unknown,
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
}

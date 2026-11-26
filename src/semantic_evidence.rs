//! Small shared semantic-evidence primitives used by offline relationship
//! analyzers.
//!
//! These helpers deliberately do not create a universal framework. They only
//! consolidate behavior already required by the interface, LLDP and neighbor
//! implementations: explicit as-of freshness, capture-time compatibility,
//! typed-fact envelope eligibility and scoped acquisition coverage.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_normalizer::CaptureOutcome;
use crate::typed_facts::{EvidenceRef, Lifecycle, TrustClass, TypedFact};

pub type Time = chrono::DateTime<chrono::FixedOffset>;

pub fn parse_time(value: &str) -> Option<Time> {
    chrono::DateTime::parse_from_rfc3339(value).ok()
}

/// Explicit consumer freshness policy. `None` for `max_observation_age`
/// means that a fact without an established validity boundary has unknown
/// freshness; it is never silently promoted to current evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FreshnessPolicy {
    pub max_observation_age_seconds: Option<u64>,
}

impl FreshnessPolicy {
    pub fn observation_age(seconds: u64) -> Self {
        Self {
            max_observation_age_seconds: Some(seconds),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFreshness {
    /// Inside an established validity window or inside the caller's explicit
    /// observation-age policy.
    Current,
    /// The evidence had an established validity window that ended before the
    /// requested analysis time. It remains useful historical evidence, not
    /// current evidence.
    Historical,
    /// Lifecycle or evidence age is stale rather than valid-history evidence.
    Stale,
    Withdrawn,
    Superseded,
    FutureObservation,
    NotYetValid,
    /// No established validity boundary exists and no explicit age policy
    /// covers the observation.
    UnknownFreshness,
}

impl EvidenceFreshness {
    pub fn is_current(self) -> bool {
        self == Self::Current
    }
}

/// Evaluate one fact at an explicit analysis time. Wall-clock time is never
/// read here. A missing `valid_until` is unknown unless the caller supplied
/// a bounded observation-age policy.
pub fn fact_freshness(fact: &TypedFact, as_of: Time, policy: FreshnessPolicy) -> EvidenceFreshness {
    match fact.lifecycle {
        Lifecycle::Withdrawn => return EvidenceFreshness::Withdrawn,
        Lifecycle::Superseded => return EvidenceFreshness::Superseded,
        Lifecycle::Stale => return EvidenceFreshness::Stale,
        Lifecycle::Active => {}
    }
    let Some(observed) = parse_time(&fact.observed_at) else {
        return EvidenceFreshness::UnknownFreshness;
    };
    if observed > as_of {
        return EvidenceFreshness::FutureObservation;
    }
    if let Some(valid_from) = fact.valid_from.as_deref().and_then(parse_time) {
        if valid_from > as_of {
            return EvidenceFreshness::NotYetValid;
        }
    }
    if let Some(valid_until) = fact.valid_until.as_deref().and_then(parse_time) {
        return if valid_until <= as_of {
            EvidenceFreshness::Historical
        } else {
            EvidenceFreshness::Current
        };
    }
    if let Some(max_age) = policy.max_observation_age_seconds {
        let age = as_of.signed_duration_since(observed);
        if age.num_seconds() >= 0 && age.num_seconds() <= max_age as i64 {
            return EvidenceFreshness::Current;
        }
        return EvidenceFreshness::Stale;
    }
    EvidenceFreshness::UnknownFreshness
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTimeCompatibility {
    Compatible,
    ExcessiveSkew,
    UnknownTime,
}

/// Compare capture observation times without requiring command execution at
/// exactly the same instant. The maximum skew is explicit consumer policy.
pub fn capture_time_compatibility(
    left: &TypedFact,
    right: &TypedFact,
    max_skew_seconds: u64,
) -> CaptureTimeCompatibility {
    let (Some(a), Some(b)) = (
        parse_time(&left.observed_at),
        parse_time(&right.observed_at),
    ) else {
        return CaptureTimeCompatibility::UnknownTime;
    };
    let skew = a.signed_duration_since(b).num_seconds().unsigned_abs();
    if skew <= max_skew_seconds {
        CaptureTimeCompatibility::Compatible
    } else {
        CaptureTimeCompatibility::ExcessiveSkew
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedFactContract<'a> {
    pub fact_kind: &'a str,
    pub subject_namespace: &'a str,
    pub operational_plane: &'a str,
    pub minimum_trust: TrustClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactEligibilityRejection {
    WrongKind,
    WrongSubjectNamespace,
    WrongTruthPlane,
    InsufficientTrust,
    InvalidEnvelope(String),
    MissingStructuredPayload,
}

/// Validate the common consumer boundary for typed operational facts. This
/// intentionally does not deserialize the payload; callers still decode and
/// validate their own typed payload after this shared envelope check.
pub fn validate_fact_envelope(
    fact: &TypedFact,
    expected: ExpectedFactContract<'_>,
) -> Result<(), FactEligibilityRejection> {
    if fact.fact_kind != expected.fact_kind {
        return Err(FactEligibilityRejection::WrongKind);
    }
    if fact.subject.namespace != expected.subject_namespace {
        return Err(FactEligibilityRejection::WrongSubjectNamespace);
    }
    if fact.subject.plane.as_deref() != Some(expected.operational_plane) {
        return Err(FactEligibilityRejection::WrongTruthPlane);
    }
    if fact.trust < expected.minimum_trust {
        return Err(FactEligibilityRejection::InsufficientTrust);
    }
    if fact.payload.fields.is_none() {
        return Err(FactEligibilityRejection::MissingStructuredPayload);
    }
    fact.validate()
        .map_err(|e| FactEligibilityRejection::InvalidEnvelope(e.to_string()))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ScopedPopulationCoverage {
    Complete,
    Partial,
    Unknown,
}

/// Acquisition coverage tied to one canonical device/scope/namespace and one
/// collector source stream. Coverage is evidence about a declared acquisition
/// scope, never a device-wide boolean manufactured by a caller.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScopedCaptureCoverage {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_namespace: Option<String>,
    pub collector_id: String,
    pub source_kind: String,
    pub capture_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation: Option<u64>,
    pub acquisition_scope: String,
    /// Typed fact population this coverage describes. Coverage for one table
    /// must never establish absence in another table merely because the
    /// device/scope/namespace matches.
    pub covered_fact_kind: String,
    pub capture_outcome: CaptureOutcome,
    pub population_coverage: ScopedPopulationCoverage,
    /// Whether the acquisition contract covers its whole declared table/scope
    /// (not merely the interfaces that happened to emit records).
    pub covers_declared_scope: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_time: Option<String>,
    /// Source-established validity boundary when one exists. Normalizers must
    /// not invent this value; consumers may apply their explicit freshness
    /// policy when it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub covered_interfaces: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub covered_forwarding_domains: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub covered_vlan_ids: BTreeSet<u32>,
    pub records_decoded: usize,
    pub malformed_records: usize,
    pub omitted_records: usize,
    pub rejected_records: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
}

impl ScopedCaptureCoverage {
    fn source_key(&self) -> (String, String, String) {
        (
            self.collector_id.clone(),
            self.source_kind.clone(),
            self.acquisition_scope.clone(),
        )
    }

    fn freshness(&self, as_of: Time, policy: FreshnessPolicy) -> EvidenceFreshness {
        let Some(observed) = self.observation_time.as_deref().and_then(parse_time) else {
            return EvidenceFreshness::UnknownFreshness;
        };
        if observed > as_of {
            return EvidenceFreshness::FutureObservation;
        }
        if let Some(until) = self.valid_until.as_deref().and_then(parse_time) {
            return if until <= as_of {
                EvidenceFreshness::Historical
            } else {
                EvidenceFreshness::Current
            };
        }
        if let Some(max_age) = policy.max_observation_age_seconds {
            let age = as_of.signed_duration_since(observed).num_seconds();
            if age >= 0 && age <= max_age as i64 {
                return EvidenceFreshness::Current;
            }
            return EvidenceFreshness::Stale;
        }
        EvidenceFreshness::UnknownFreshness
    }

    pub fn usable_for_absence(&self, as_of: Time, policy: FreshnessPolicy) -> bool {
        self.capture_outcome == CaptureOutcome::Complete
            && self.population_coverage == ScopedPopulationCoverage::Complete
            && self.freshness(as_of, policy).is_current()
            && self.malformed_records == 0
            && self.omitted_records == 0
            && self.rejected_records == 0
    }
}

/// Select usable coverage by first choosing the newest generation inside each
/// independent collector/source stream and only then testing completeness and
/// freshness. Generations from unrelated collectors are never compared as one
/// ordered stream. A newer incomplete capture therefore masks older complete
/// coverage from the same source, while an independent complete source can
/// still establish coverage.
pub fn select_usable_coverage<'a, I>(
    candidates: I,
    as_of: Time,
    policy: FreshnessPolicy,
) -> Option<&'a ScopedCaptureCoverage>
where
    I: IntoIterator<Item = &'a ScopedCaptureCoverage>,
{
    let mut by_source: BTreeMap<(String, String, String), Vec<&ScopedCaptureCoverage>> =
        BTreeMap::new();
    for coverage in candidates {
        by_source
            .entry(coverage.source_key())
            .or_default()
            .push(coverage);
    }
    let mut usable = Vec::new();
    for (_source, mut group) in by_source {
        group.sort_by(|a, b| {
            let ag = a.source_generation.unwrap_or(0);
            let bg = b.source_generation.unwrap_or(0);
            (ag, &a.observation_time, &a.capture_id).cmp(&(bg, &b.observation_time, &b.capture_id))
        });
        if let Some(newest) = group.last() {
            // Equal generation/time with distinct capture identities is
            // ambiguous evidence within one source stream. Deterministically
            // sorting by capture id must not promote an arbitrary winner.
            let ambiguous_newest = group.iter().any(|coverage| {
                coverage.capture_id != newest.capture_id
                    && coverage.source_generation == newest.source_generation
                    && coverage.observation_time == newest.observation_time
            });
            if !ambiguous_newest && newest.usable_for_absence(as_of, policy) {
                usable.push(*newest);
            }
        }
    }
    usable.sort_by(|a, b| a.source_key().cmp(&b.source_key()));
    usable.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(
        collector: &str,
        generation: u64,
        observed: &str,
        population: ScopedPopulationCoverage,
    ) -> ScopedCaptureCoverage {
        ScopedCaptureCoverage {
            device_id: "leaf1".into(),
            scope: None,
            network_namespace: None,
            collector_id: collector.into(),
            source_kind: "cli".into(),
            capture_id: format!("capture-{collector}-{generation}"),
            source_generation: Some(generation),
            acquisition_scope: "test/table".into(),
            covered_fact_kind: "test.fact".into(),
            capture_outcome: CaptureOutcome::Complete,
            population_coverage: population,
            covers_declared_scope: true,
            observation_time: Some(observed.into()),
            valid_until: Some("2026-01-01T00:10:00Z".into()),
            covered_interfaces: BTreeSet::new(),
            covered_forwarding_domains: BTreeSet::new(),
            covered_vlan_ids: BTreeSet::new(),
            records_decoded: 0,
            malformed_records: 0,
            omitted_records: 0,
            rejected_records: 0,
            evidence_refs: vec![],
        }
    }

    fn as_of() -> Time {
        parse_time("2026-01-01T00:01:00Z").unwrap()
    }

    #[test]
    fn newest_generation_is_selected_before_completeness_within_each_source() {
        let older = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Complete,
        );
        let newer = coverage(
            "collector-a",
            2,
            "2026-01-01T00:00:30Z",
            ScopedPopulationCoverage::Partial,
        );
        assert!(
            select_usable_coverage([&older, &newer], as_of(), FreshnessPolicy::default()).is_none(),
            "older complete evidence must not mask newer incomplete evidence"
        );

        let unrelated_newer = coverage(
            "collector-b",
            99,
            "2026-01-01T00:00:30Z",
            ScopedPopulationCoverage::Partial,
        );
        let selected = select_usable_coverage(
            [&older, &unrelated_newer],
            as_of(),
            FreshnessPolicy::default(),
        );
        assert!(selected.is_some_and(|c| c.capture_id == older.capture_id));
    }

    #[test]
    fn ambiguous_same_generation_captures_do_not_produce_arbitrary_coverage() {
        let mut a = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Complete,
        );
        a.capture_id = "capture-a".into();
        let mut b = a.clone();
        b.capture_id = "capture-b".into();
        assert!(select_usable_coverage([&a, &b], as_of(), FreshnessPolicy::default()).is_none());
    }

    #[test]
    fn unknown_validity_requires_explicit_age_policy() {
        let mut cov = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Complete,
        );
        cov.valid_until = None;
        assert!(select_usable_coverage([&cov], as_of(), FreshnessPolicy::default()).is_none());
        assert!(
            select_usable_coverage([&cov], as_of(), FreshnessPolicy::observation_age(300))
                .is_some()
        );
        cov.observation_time = Some("2025-12-31T23:00:00Z".into());
        assert!(
            select_usable_coverage([&cov], as_of(), FreshnessPolicy::observation_age(300))
                .is_none()
        );
    }
}

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

pub(crate) fn semantic_time_key(value: Option<&str>) -> Option<i128> {
    value
        .and_then(parse_time)
        .map(|time| time.timestamp_micros() as i128)
}

/// Explicit consumer freshness policy. `None` for `max_observation_age`
/// means that evidence without an established validity boundary has unknown
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

/// Evaluate observation metadata at an explicit analysis time. This is the
/// single temporal contract shared by typed facts, acquisition coverage and
/// forwarding-domain bindings. Wall-clock time is never read here. A missing
/// `valid_until` is unknown unless the caller supplied a bounded
/// observation-age policy.
pub fn observation_freshness(
    lifecycle: Lifecycle,
    observed_at: &str,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    as_of: Time,
    policy: FreshnessPolicy,
) -> EvidenceFreshness {
    match lifecycle {
        Lifecycle::Withdrawn => return EvidenceFreshness::Withdrawn,
        Lifecycle::Superseded => return EvidenceFreshness::Superseded,
        Lifecycle::Stale => return EvidenceFreshness::Stale,
        Lifecycle::Active => {}
    }
    let Some(observed) = parse_time(observed_at) else {
        return EvidenceFreshness::UnknownFreshness;
    };
    if observed > as_of {
        return EvidenceFreshness::FutureObservation;
    }
    if let Some(valid_from) = valid_from.and_then(parse_time) {
        if valid_from > as_of {
            return EvidenceFreshness::NotYetValid;
        }
    }
    if let Some(valid_until) = valid_until.and_then(parse_time) {
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

/// Evaluate one fact at an explicit analysis time.
pub fn fact_freshness(fact: &TypedFact, as_of: Time, policy: FreshnessPolicy) -> EvidenceFreshness {
    observation_freshness(
        fact.lifecycle,
        &fact.observed_at,
        fact.valid_from.as_deref(),
        fact.valid_until.as_deref(),
        as_of,
        policy,
    )
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

/// Compare one typed fact's observation time with coverage/evidence metadata
/// using the same explicit skew policy.
pub fn evidence_time_compatibility(
    fact: &TypedFact,
    evidence_observed_at: Option<&str>,
    max_skew_seconds: u64,
) -> CaptureTimeCompatibility {
    let (Some(a), Some(b)) = (
        parse_time(&fact.observed_at),
        evidence_observed_at.and_then(parse_time),
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

/// Declared acquisition-boundary contract. This is intentionally independent
/// of the records that happened to decode successfully: a complete empty table
/// is positive evidence only when the command/subscription contract establishes
/// that the scoped table was acquired.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionScopeContract {
    /// The qualified source contract acquired the complete table for this
    /// canonical device/scope/network namespace (for example a qualified
    /// whole-table command in one network namespace).
    CompleteScopedTable,
    /// The acquisition is explicitly a subset. It cannot establish absence in
    /// the complete table, though it may cover a separately qualified subset.
    ScopedSubset,
    /// The acquisition boundary is missing, malformed or not established.
    Unknown,
}

/// Acquisition coverage tied to one canonical device/scope/namespace and one
/// collector source stream. Coverage is evidence about a declared acquisition
/// scope, never a device-wide boolean manufactured by a caller. The
/// `observed_*` sets describe the successfully observed population only; they
/// are not the acquisition boundary.
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
    /// Qualified declared acquisition boundary, independent of decoded rows.
    pub declared_scope: AcquisitionScopeContract,
    pub capture_outcome: CaptureOutcome,
    pub population_coverage: ScopedPopulationCoverage,
    /// The source positively established an empty in-scope population. Empty
    /// output caused by a missing/malformed collection is not positive empty.
    pub positive_empty_population: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_time: Option<String>,
    /// Source-established validity boundary when one exists. Normalizers must
    /// not invent this value; consumers may apply their explicit freshness
    /// policy when it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Successfully observed population axes. These are diagnostic/narrowing
    /// evidence only; they never expand a subset or unknown acquisition into
    /// whole-table coverage.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub observed_interfaces: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub observed_forwarding_domains: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub observed_vlan_ids: BTreeSet<u32>,
    pub records_decoded: usize,
    pub malformed_records: usize,
    pub omitted_records: usize,
    pub rejected_records: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
}

impl ScopedCaptureCoverage {
    pub fn freshness(&self, as_of: Time, policy: FreshnessPolicy) -> EvidenceFreshness {
        let Some(observed) = self.observation_time.as_deref() else {
            return EvidenceFreshness::UnknownFreshness;
        };
        observation_freshness(
            Lifecycle::Active,
            observed,
            None,
            self.valid_until.as_deref(),
            as_of,
            policy,
        )
    }

    pub fn usable_for_absence(&self, as_of: Time, policy: FreshnessPolicy) -> bool {
        self.capture_outcome == CaptureOutcome::Complete
            && self.population_coverage == ScopedPopulationCoverage::Complete
            && self.declared_scope == AcquisitionScopeContract::CompleteScopedTable
            && self.freshness(as_of, policy).is_current()
            && self.malformed_records == 0
            && self.omitted_records == 0
            && self.rejected_records == 0
    }
}

/// Domain-neutral stream identity/ordering surface implemented by concrete
/// coverage evidence types. Implementations remain responsible for their own
/// completeness semantics; this shared selector owns replay consolidation,
/// same-contradictory-capture detection, future exclusion and deterministic
/// newest-capture selection.
pub trait CoverageStreamEvidence {
    fn stream_key(&self) -> (String, String, String);
    fn capture_id(&self) -> Option<&str>;
    fn source_generation(&self) -> Option<u64>;
    fn observation_time(&self) -> Option<&str>;
    fn usable_for_absence(&self, as_of: Time, policy: FreshnessPolicy) -> bool;
    /// Canonical semantic content used to distinguish exact replay from
    /// contradictory objects sharing one capture identity. Timestamp keys must
    /// be normalized instants, not raw RFC-3339 strings.
    fn replay_signature(&self) -> String;
}

impl CoverageStreamEvidence for ScopedCaptureCoverage {
    fn stream_key(&self) -> (String, String, String) {
        (
            self.collector_id.clone(),
            self.source_kind.clone(),
            self.acquisition_scope.clone(),
        )
    }

    fn capture_id(&self) -> Option<&str> {
        Some(&self.capture_id)
    }

    fn source_generation(&self) -> Option<u64> {
        self.source_generation
    }

    fn observation_time(&self) -> Option<&str> {
        self.observation_time.as_deref()
    }

    fn usable_for_absence(&self, as_of: Time, policy: FreshnessPolicy) -> bool {
        self.usable_for_absence(as_of, policy)
    }

    fn replay_signature(&self) -> String {
        serde_json::json!({
            "device_id": self.device_id,
            "scope": self.scope,
            "network_namespace": self.network_namespace,
            "stream": self.stream_key(),
            "capture_id": self.capture_id,
            "source_generation": self.source_generation,
            "covered_fact_kind": self.covered_fact_kind,
            "declared_scope": self.declared_scope,
            "capture_outcome": self.capture_outcome,
            "population_coverage": self.population_coverage,
            "positive_empty_population": self.positive_empty_population,
            "observation_time": semantic_time_key(self.observation_time.as_deref()),
            "valid_until": semantic_time_key(self.valid_until.as_deref()),
            "observed_interfaces": self.observed_interfaces,
            "observed_forwarding_domains": self.observed_forwarding_domains,
            "observed_vlan_ids": self.observed_vlan_ids,
            "records_decoded": self.records_decoded,
            "malformed_records": self.malformed_records,
            "omitted_records": self.omitted_records,
            "rejected_records": self.rejected_records,
            "evidence_refs": self.evidence_refs,
        })
        .to_string()
    }
}

#[derive(Clone, Debug)]
struct StreamCandidate<'a, T: CoverageStreamEvidence + ?Sized> {
    coverage: &'a T,
    capture_id: Option<String>,
    generation: Option<u64>,
    observation: Option<Time>,
    signature: String,
}

impl<'a, T: CoverageStreamEvidence + ?Sized> StreamCandidate<'a, T> {
    fn new(coverage: &'a T) -> Self {
        Self {
            coverage,
            capture_id: coverage.capture_id().map(str::to_string),
            generation: coverage.source_generation(),
            observation: coverage.observation_time().and_then(parse_time),
            signature: coverage.replay_signature(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamOrder {
    Before,
    After,
    Ambiguous,
}

fn compare_stream_order<T: CoverageStreamEvidence + ?Sized>(
    left: &StreamCandidate<'_, T>,
    right: &StreamCandidate<'_, T>,
) -> StreamOrder {
    if let (Some(left_generation), Some(right_generation)) = (left.generation, right.generation) {
        if left_generation != right_generation {
            return if left_generation < right_generation {
                StreamOrder::Before
            } else {
                StreamOrder::After
            };
        }
    }
    if let (Some(left_time), Some(right_time)) = (left.observation, right.observation) {
        if left_time != right_time {
            return if left_time < right_time {
                StreamOrder::Before
            } else {
                StreamOrder::After
            };
        }
    }
    // Unknown generations are never coerced to zero/any established generation.
    // Equal timestamps, missing timestamps and missing generations leave the
    // ordering explicitly ambiguous rather than input-order dependent.
    StreamOrder::Ambiguous
}

/// Consolidate exact replay and select usable coverage by first choosing the
/// newest temporally eligible capture inside each independent collector/source
/// stream and only then testing completeness and freshness. Generations from
/// unrelated collectors are never compared as one ordered stream. Distinct
/// captures whose ordering cannot be established are ambiguous; contradictory
/// objects sharing one capture identity invalidate that stream rather than
/// producing an input-order-dependent winner.
pub fn select_usable_coverage<'a, T, I>(
    candidates: I,
    as_of: Time,
    policy: FreshnessPolicy,
) -> Option<&'a T>
where
    T: CoverageStreamEvidence + ?Sized,
    I: IntoIterator<Item = &'a T>,
{
    let mut by_source: BTreeMap<(String, String, String), Vec<StreamCandidate<'a, T>>> =
        BTreeMap::new();
    for coverage in candidates {
        by_source
            .entry(coverage.stream_key())
            .or_default()
            .push(StreamCandidate::new(coverage));
    }

    let mut usable = Vec::new();
    for (_source, group) in by_source {
        // Same-capture objects are consolidated before temporal selection.
        // Identical semantic replay is one capture; any semantic difference is
        // contradictory evidence even when population completeness differs only.
        let mut consolidated: Vec<StreamCandidate<'a, T>> = Vec::new();
        let mut contradictory_stream = false;
        for candidate in group {
            if let Some(capture_id) = &candidate.capture_id {
                if let Some(position) = consolidated.iter().position(|existing| {
                    existing.capture_id.as_deref() == Some(capture_id.as_str())
                }) {
                    if consolidated[position].signature == candidate.signature {
                        continue;
                    }
                    contradictory_stream = true;
                    break;
                }
            }
            consolidated.push(candidate);
        }
        if contradictory_stream {
            continue;
        }

        // Future observations are excluded before historical selection. Missing
        // or malformed times cannot be proven future; they also cannot be used
        // for absence, but a known generation can still conservatively mask an
        // older capture as newer unordered/malformed evidence.
        let eligible: Vec<_> = consolidated
            .into_iter()
            .filter(|candidate| candidate.observation.is_none_or(|time| time <= as_of))
            .collect();
        if eligible.is_empty() {
            continue;
        }

        let mut maximal = Vec::new();
        for (index, candidate) in eligible.iter().enumerate() {
            let has_newer = eligible.iter().enumerate().any(|(other_index, other)| {
                other_index != index && compare_stream_order(other, candidate) == StreamOrder::After
            });
            if !has_newer {
                maximal.push(candidate);
            }
        }
        if maximal.len() != 1 {
            continue;
        }
        let newest = maximal[0];
        if newest.coverage.usable_for_absence(as_of, policy) {
            usable.push(newest.coverage);
        }
    }
    usable.sort_by(|a, b| {
        (a.stream_key(), a.capture_id().unwrap_or_default())
            .cmp(&(b.stream_key(), b.capture_id().unwrap_or_default()))
    });
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
            declared_scope: AcquisitionScopeContract::CompleteScopedTable,
            capture_outcome: CaptureOutcome::Complete,
            population_coverage: population,
            positive_empty_population: false,
            observation_time: Some(observed.into()),
            valid_until: Some("2026-01-01T00:10:00Z".into()),
            observed_interfaces: BTreeSet::new(),
            observed_forwarding_domains: BTreeSet::new(),
            observed_vlan_ids: BTreeSet::new(),
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

    #[test]
    fn equivalent_timestamp_representations_compare_as_instants() {
        let mut a = coverage(
            "collector-a",
            1,
            "2026-01-01T02:00:00+02:00",
            ScopedPopulationCoverage::Complete,
        );
        a.capture_id = "capture-same".into();
        let mut b = a.clone();
        b.observation_time = Some("2026-01-01T00:00:00.000Z".into());
        let selected = select_usable_coverage([&a, &b], as_of(), FreshnessPolicy::default());
        assert!(
            selected.is_some(),
            "same capture replay with equivalent timestamp representations must consolidate"
        );

        let mut c = a.clone();
        c.capture_id = "capture-other".into();
        assert!(
            select_usable_coverage([&a, &c], as_of(), FreshnessPolicy::default()).is_none(),
            "distinct captures at the same instant remain ambiguous even when offsets differ"
        );
    }

    #[test]
    fn fractional_second_representations_do_not_change_timestamp_order() {
        let mut older = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00.500Z",
            ScopedPopulationCoverage::Complete,
        );
        older.capture_id = "capture-older".into();
        let mut newer = coverage(
            "collector-a",
            2,
            "2026-01-01T00:00:30+00:00",
            ScopedPopulationCoverage::Complete,
        );
        newer.capture_id = "capture-newer".into();
        let selected =
            select_usable_coverage([&older, &newer], as_of(), FreshnessPolicy::default());
        assert!(selected.is_some_and(|c| c.capture_id == "capture-newer"));

        let mut same_instant = older.clone();
        same_instant.capture_id = "capture-same-instant".into();
        same_instant.source_generation = older.source_generation;
        same_instant.observation_time = Some("2026-01-01T01:00:00.500+01:00".into());
        assert!(
            select_usable_coverage([&older, &same_instant], as_of(), FreshnessPolicy::default())
                .is_none(),
            "fraction/offset spelling must not manufacture a newest distinct capture"
        );
    }

    #[test]
    fn future_capture_is_excluded_before_historical_selection() {
        let mut older = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Complete,
        );
        older.capture_id = "capture-old".into();
        let mut future = coverage(
            "collector-a",
            2,
            "2026-01-01T00:02:00Z",
            ScopedPopulationCoverage::Partial,
        );
        future.capture_id = "capture-future".into();
        let selected =
            select_usable_coverage([&older, &future], as_of(), FreshnessPolicy::default());
        assert!(
            selected.is_some_and(|c| c.capture_id == "capture-old"),
            "future partial evidence must not mask an older complete capture at a historical as_of"
        );
    }

    #[test]
    fn contradictory_same_capture_coverage_is_order_independent_and_unusable() {
        let mut complete = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Complete,
        );
        complete.capture_id = "capture-contradicted".into();
        let mut partial = complete.clone();
        partial.population_coverage = ScopedPopulationCoverage::Partial;
        for ordered in [[&complete, &partial], [&partial, &complete]] {
            assert!(
                select_usable_coverage(ordered, as_of(), FreshnessPolicy::default()).is_none(),
                "complete/partial objects sharing one capture identity contradict one another"
            );
        }
    }

    #[test]
    fn unknown_generation_is_not_coerced_to_an_established_generation() {
        let mut known = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:00Z",
            ScopedPopulationCoverage::Partial,
        );
        known.capture_id = "capture-known".into();
        let mut unknown = coverage(
            "collector-a",
            1,
            "2026-01-01T00:00:30Z",
            ScopedPopulationCoverage::Complete,
        );
        unknown.capture_id = "capture-unknown-generation".into();
        unknown.source_generation = None;
        let selected =
            select_usable_coverage([&known, &unknown], as_of(), FreshnessPolicy::default());
        assert!(
            selected.is_some_and(|c| c.capture_id == unknown.capture_id),
            "an unknown generation must not lose as though it were generation zero; parsed time can still order it"
        );

        known.population_coverage = ScopedPopulationCoverage::Complete;
        unknown.observation_time = known.observation_time.clone();
        assert!(
            select_usable_coverage([&known, &unknown], as_of(), FreshnessPolicy::default())
                .is_none(),
            "same-time evidence with one unknown generation has no established newest capture"
        );
    }
}

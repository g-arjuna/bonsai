//! Bounded read-only queries over derived forwarding-domain evidence.
//!
//! These queries never mutate topology, never merge distinct bindings into an
//! authoritative-looking composite and never infer identity from names. Each
//! answer preserves per-binding granularity with provenance so callers can
//! distinguish exact replay, corroborating observations and genuinely
//! conflicting or ambiguous domain attachments.

use serde::Serialize;
use std::collections::BTreeSet;

use crate::neighbor_reconciliation::ForwardingDomainEvidence;
use crate::semantic_evidence::{FreshnessPolicy, parse_time};

/// Absolute bound on returned bindings per query. Oversized evidence inputs
/// are truncated deterministically and counted, never silently dropped.
pub const MAX_QUERY_RESULTS: usize = 256;

#[derive(Clone, Debug)]
pub struct ScopedQuery<'a> {
    pub device_id: &'a str,
    pub scope: Option<&'a str>,
    pub network_namespace: Option<&'a str>,
    pub as_of: &'a str,
    pub freshness: FreshnessPolicy,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DomainAttachment {
    /// Interface appears in the domain's forwarding/bridge population.
    Forwarding,
    /// Interface is a routed/SVI attachment of the domain.
    Routed,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct InterfaceDomainMembership {
    pub domain_id: String,
    pub vlan_id: Option<u32>,
    pub vrf: Option<String>,
    pub attachment: DomainAttachment,
    pub matched_via_structured_key: bool,
    pub observation_time: Option<String>,
    pub capture_id: Option<String>,
    pub evidence_refs: Vec<crate::typed_facts::EvidenceRef>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct InterfaceDomainAnswer {
    /// All current bindings attaching the interface, in deterministic order.
    /// Multiple memberships are explicit evidence — never collapsed.
    pub memberships: Vec<InterfaceDomainMembership>,
    /// Bindings that referenced the interface but were excluded because
    /// provenance or temporal eligibility could not be established.
    pub ineligible_evidence: usize,
    /// Matching bindings omitted by the result bound.
    pub results_omitted: usize,
}

fn same_scope(query: &ScopedQuery<'_>, ev: &ForwardingDomainEvidence) -> bool {
    ev.device_id == query.device_id
        && ev.scope.as_deref() == query.scope
        && ev.network_namespace.as_deref() == query.network_namespace
}

fn interface_matches(
    ev: &ForwardingDomainEvidence,
    interface: &str,
) -> Vec<(DomainAttachment, bool)> {
    let mut out = Vec::new();
    let structured_hit = |keys: &BTreeSet<crate::interface_relationships::InterfaceEntityKey>| {
        keys.iter().any(|key| key.canonical_interface == interface)
    };
    if ev.forwarding_interfaces.contains(interface) {
        out.push((DomainAttachment::Forwarding, false));
    } else if structured_hit(&ev.forwarding_interface_keys) {
        out.push((DomainAttachment::Forwarding, true));
    }
    if ev.routed_interfaces.contains(interface) {
        out.push((DomainAttachment::Routed, false));
    } else if structured_hit(&ev.routed_interface_keys) {
        out.push((DomainAttachment::Routed, true));
    }
    // Explicitly established equivalence evidence can resolve an alias, but
    // equivalence is directional attachment evidence, not identity collapse:
    // the alias itself must have been positively established in the binding.
    for (canonical, aliases) in &ev.interface_equivalents {
        if aliases.contains(interface) && ev.forwarding_interfaces.contains(canonical) {
            out.push((DomainAttachment::Forwarding, false));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Which forwarding domains attach one interface at an explicit analysis
/// time. Interface equality uses exact names or independently established
/// structured keys; no case folding or spelling inference.
pub fn domains_for_interface(
    evidence: &[ForwardingDomainEvidence],
    query: &ScopedQuery<'_>,
    interface: &str,
) -> Result<InterfaceDomainAnswer, String> {
    let as_of = parse_time(query.as_of).ok_or_else(|| "as_of is not RFC-3339".to_string())?;
    let mut answer = InterfaceDomainAnswer::default();
    for ev in evidence {
        if !same_scope(query, ev) {
            continue;
        }
        let attachments = interface_matches(ev, interface);
        if attachments.is_empty() {
            continue;
        }
        if !crate::neighbor_reconciliation::domain_evidence_current(ev, as_of, query.freshness) {
            answer.ineligible_evidence += 1;
            continue;
        }
        for (attachment, via_key) in attachments {
            if answer.memberships.len() >= MAX_QUERY_RESULTS {
                answer.results_omitted += 1;
                continue;
            }
            answer.memberships.push(InterfaceDomainMembership {
                domain_id: ev.domain_id.clone(),
                vlan_id: ev.vlan_id,
                vrf: ev.vrf.clone(),
                attachment,
                matched_via_structured_key: via_key,
                observation_time: ev.observation_time.clone(),
                capture_id: ev.capture_id.clone(),
                evidence_refs: ev.evidence_refs.clone(),
            });
        }
    }
    answer.memberships.sort_by(|a, b| {
        (&a.domain_id, a.vlan_id, a.attachment, &a.capture_id).cmp(&(
            &b.domain_id,
            b.vlan_id,
            b.attachment,
            &b.capture_id,
        ))
    });
    answer.memberships.dedup_by(|a, b| {
        a.domain_id == b.domain_id
            && a.vlan_id == b.vlan_id
            && a.attachment == b.attachment
            && a.capture_id == b.capture_id
    });
    Ok(answer)
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DomainBindingView {
    pub domain_id: String,
    pub vlan_id: Option<u32>,
    pub vrf: Option<String>,
    pub forwarding_interfaces: Vec<String>,
    pub routed_interfaces: Vec<String>,
    pub observation_time: Option<String>,
    pub capture_id: Option<String>,
    pub evidence_refs: Vec<crate::typed_facts::EvidenceRef>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DomainBindingsAnswer {
    /// Current bindings for the scoped domain, one view per evidence object.
    /// Bindings from different captures or with contradictory VLAN/VRF
    /// context remain separate — the query never manufactures consensus.
    pub bindings: Vec<DomainBindingView>,
    pub ineligible_evidence: usize,
    pub results_omitted: usize,
}

/// Current evidence bindings for one scoped forwarding domain, optionally
/// restricted to one VLAN context. Empty means no temporally eligible
/// evidence — unknown, never a positive empty-domain assertion.
pub fn bindings_for_domain(
    evidence: &[ForwardingDomainEvidence],
    query: &ScopedQuery<'_>,
    domain_id: &str,
    vlan: Option<u32>,
) -> Result<DomainBindingsAnswer, String> {
    let as_of = parse_time(query.as_of).ok_or_else(|| "as_of is not RFC-3339".to_string())?;
    let mut answer = DomainBindingsAnswer::default();
    for ev in evidence {
        if !same_scope(query, ev) || ev.domain_id != domain_id {
            continue;
        }
        if let Some(vlan) = vlan
            && ev.vlan_id != Some(vlan)
        {
            continue;
        }
        if !crate::neighbor_reconciliation::domain_evidence_current(ev, as_of, query.freshness) {
            answer.ineligible_evidence += 1;
            continue;
        }
        if answer.bindings.len() >= MAX_QUERY_RESULTS {
            answer.results_omitted += 1;
            continue;
        }
        answer.bindings.push(DomainBindingView {
            domain_id: ev.domain_id.clone(),
            vlan_id: ev.vlan_id,
            vrf: ev.vrf.clone(),
            forwarding_interfaces: ev.forwarding_interfaces.iter().cloned().collect(),
            routed_interfaces: ev.routed_interfaces.iter().cloned().collect(),
            observation_time: ev.observation_time.clone(),
            capture_id: ev.capture_id.clone(),
            evidence_refs: ev.evidence_refs.clone(),
        });
    }
    answer.bindings.sort_by(|a, b| {
        (&a.domain_id, a.vlan_id, &a.capture_id).cmp(&(&b.domain_id, b.vlan_id, &b.capture_id))
    });
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
    use crate::typed_facts::Lifecycle;
    use crate::vlan_normalizer::{VlanObservation, VlanSourceProfile, normalize_vlan};

    fn query<'a>() -> ScopedQuery<'a> {
        ScopedQuery {
            device_id: "device-leaf02",
            scope: None,
            network_namespace: None,
            as_of: "2021-06-07T00:00:30+00:00",
            freshness: FreshnessPolicy::observation_age(120),
        }
    }

    fn eos_bindings() -> Vec<ForwardingDomainEvidence> {
        let input = include_str!("../tests/fixtures/bn024/vlan/eos-leaf02-show-vlan-brief.json");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::EosShowVlanBriefJson,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "device-leaf02",
                "collector-test",
                "eos-vlan-capture",
                "2021-06-07T00:00:05+00:00",
            ),
        });
        crate::forwarding_domain_derivation::derive_forwarding_domains(
            &vlan.facts,
            crate::forwarding_domain_derivation::ForwardingDomainDerivationPolicy {
                freshness: FreshnessPolicy::observation_age(120),
                max_capture_skew_seconds: 60,
            },
            "2021-06-07T00:00:30+00:00",
        )
        .unwrap()
        .evidence
    }

    #[test]
    fn trunk_interface_reports_all_observed_domain_memberships() {
        let bindings = eos_bindings();
        let answer = domains_for_interface(&bindings, &query(), "Port-Channel1").unwrap();
        let vlans: BTreeSet<_> = answer
            .memberships
            .iter()
            .filter_map(|m| m.vlan_id)
            .collect();
        assert!(
            vlans.is_superset(&BTreeSet::from([1, 10, 30, 1006, 4094])),
            "trunk memberships must all be preserved: {vlans:?}"
        );
        assert!(
            answer
                .memberships
                .iter()
                .all(|m| m.attachment == DomainAttachment::Forwarding)
        );
        assert!(
            answer
                .memberships
                .iter()
                .all(|m| !m.evidence_refs.is_empty())
        );
    }

    #[test]
    fn single_domain_interface_reports_one_membership() {
        let bindings = eos_bindings();
        let answer = domains_for_interface(&bindings, &query(), "Port-Channel3").unwrap();
        assert_eq!(answer.memberships.len(), 1);
        assert_eq!(answer.memberships[0].vlan_id, Some(10));
    }

    #[test]
    fn unknown_interface_has_empty_answer_not_positive_absence() {
        let bindings = eos_bindings();
        let answer = domains_for_interface(&bindings, &query(), "Ethernet999").unwrap();
        assert!(answer.memberships.is_empty());
        assert_eq!(answer.ineligible_evidence, 0);
    }

    #[test]
    fn stale_binding_is_counted_ineligible_not_returned() {
        let mut bindings = eos_bindings();
        for binding in &mut bindings {
            binding.lifecycle = Lifecycle::Stale;
        }
        let answer = domains_for_interface(&bindings, &query(), "Port-Channel3").unwrap();
        assert!(answer.memberships.is_empty());
        assert!(answer.ineligible_evidence > 0);
    }

    #[test]
    fn different_scope_never_matches() {
        let bindings = eos_bindings();
        let mut q = query();
        q.scope = Some("other-tenant");
        let answer = domains_for_interface(&bindings, &q, "Port-Channel3").unwrap();
        assert!(answer.memberships.is_empty());
    }

    #[test]
    fn domain_binding_views_preserve_vlan_contexts() {
        let bindings = eos_bindings();
        let answer = bindings_for_domain(&bindings, &query(), "vlan/30", None).unwrap();
        assert_eq!(answer.bindings.len(), 1);
        assert_eq!(answer.bindings[0].vlan_id, Some(30));
        assert!(
            answer.bindings[0]
                .forwarding_interfaces
                .contains(&"Port-Channel4".to_string())
        );
        let none = bindings_for_domain(&bindings, &query(), "vlan/30", Some(10)).unwrap();
        assert!(
            none.bindings.is_empty(),
            "VLAN filter must not cross contexts"
        );
    }
}

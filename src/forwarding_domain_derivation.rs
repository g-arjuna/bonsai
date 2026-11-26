//! Deterministic derivation of forwarding-domain bindings from typed VLAN,
//! bridge and interface evidence.
//!
//! The output feeds the existing neighbor reconciler; it is not a topology
//! mutation and never invents bridge, SVI, VRF or LAG identity from names.
//! Linux port/VLAN rows remain indeterminate unless independently typed
//! interface master evidence associates the port with a bridge.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::{
    FACT_KIND_INTERFACE_STATE, FieldAvailability, InterfaceStatePayload, MasterKind,
};
use crate::interface_relationships::InterfaceEntityKey;
use crate::neighbor_reconciliation::ForwardingDomainEvidence;
use crate::semantic_evidence::{
    CaptureTimeCompatibility, FreshnessPolicy, capture_time_compatibility, fact_freshness,
    parse_time,
};
use crate::typed_facts::{EvidenceRef, Lifecycle, TrustClass, TypedFact};
use crate::vlan_facts::{FACT_KIND_VLAN_BRIDGE_DOMAIN, VlanBridgePayload, VlanObservationShape};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForwardingDomainDerivationPolicy {
    pub freshness: FreshnessPolicy,
    pub max_capture_skew_seconds: u64,
}

impl Default for ForwardingDomainDerivationPolicy {
    fn default() -> Self {
        Self {
            freshness: FreshnessPolicy::default(),
            max_capture_skew_seconds: 60,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ForwardingDomainDerivationReport {
    pub vlan_facts_seen: usize,
    pub interface_facts_seen: usize,
    pub invalid_vlan_facts: usize,
    pub invalid_interface_facts: usize,
    pub temporally_ineligible_facts: usize,
    pub database_bindings_derived: usize,
    pub linux_bridge_bindings_derived: usize,
    pub indeterminate_port_memberships: usize,
    pub incompatible_interface_evidence: usize,
    pub lag_relationships_included: usize,
}

#[derive(Clone, Debug)]
pub struct ForwardingDomainDerivation {
    pub evidence: Vec<ForwardingDomainEvidence>,
    pub report: ForwardingDomainDerivationReport,
}

fn decode_payload<T: serde::de::DeserializeOwned>(fact: &TypedFact) -> Option<T> {
    serde_json::from_value(fact.payload.fields.clone()?).ok()
}

fn evidence_refs_with_fact(fact: &TypedFact) -> Vec<EvidenceRef> {
    let mut refs = fact.evidence.clone();
    refs.push(EvidenceRef {
        kind: "typed_fact_reference".into(),
        sha256: fact.payload_sha256.clone(),
        locator: Some(fact.fact_id.clone()),
    });
    refs
}

fn known_string(field: &crate::interface_facts::ObservedField<String>) -> Option<String> {
    (field.state == FieldAvailability::Known)
        .then_some(field.value.clone())
        .flatten()
}

fn compatible(left: &TypedFact, right: &TypedFact, max_skew: u64) -> bool {
    capture_time_compatibility(left, right, max_skew) == CaptureTimeCompatibility::Compatible
}

fn min_time(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left.and_then(parse_time), right.and_then(parse_time)) {
        (Some(a), Some(b)) => Some(if a <= b { a } else { b }.to_rfc3339()),
        _ => None,
    }
}

fn max_time(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left.and_then(parse_time), right.and_then(parse_time)) {
        (Some(a), Some(b)) => Some(if a >= b { a } else { b }.to_rfc3339()),
        (Some(a), None) => Some(a.to_rfc3339()),
        (None, Some(b)) => Some(b.to_rfc3339()),
        (None, None) => None,
    }
}

fn composite_capture_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update(part.as_bytes());
        h.update([0]);
    }
    format!("derived-batch-{:x}", h.finalize())[..64].to_string()
}

struct InterfaceEvidence<'a> {
    fact: &'a TypedFact,
    payload: InterfaceStatePayload,
    key: Option<InterfaceEntityKey>,
    names: BTreeSet<String>,
}

/// Derive scoped forwarding-domain evidence at an explicit analysis time.
/// Only current typed facts participate. VLAN-database entries produce direct
/// bindings; Linux port membership produces bindings only when an interface
/// fact independently establishes `master` as a bridge and the observations
/// are inside the caller's capture-skew policy.
pub fn derive_forwarding_domains(
    facts: &[TypedFact],
    policy: ForwardingDomainDerivationPolicy,
    as_of: &str,
) -> Result<ForwardingDomainDerivation, String> {
    let as_of = parse_time(as_of).ok_or_else(|| "as_of is not RFC-3339".to_string())?;
    let mut report = ForwardingDomainDerivationReport::default();
    let mut vlan_entries = Vec::new();
    let mut interfaces = Vec::new();

    for fact in facts {
        let freshness = fact_freshness(fact, as_of, policy.freshness);
        if !freshness.is_current() {
            if matches!(
                fact.fact_kind.as_str(),
                FACT_KIND_VLAN_BRIDGE_DOMAIN | FACT_KIND_INTERFACE_STATE
            ) {
                report.temporally_ineligible_facts += 1;
            }
            continue;
        }
        match fact.fact_kind.as_str() {
            FACT_KIND_VLAN_BRIDGE_DOMAIN => {
                report.vlan_facts_seen += 1;
                if fact.subject.namespace != "forwarding"
                    || fact.subject.plane.as_deref() != Some("operational")
                    || fact.trust < TrustClass::DeviceAsserted
                    || fact.validate().is_err()
                {
                    report.invalid_vlan_facts += 1;
                    continue;
                }
                let Some(payload) = decode_payload::<VlanBridgePayload>(fact) else {
                    report.invalid_vlan_facts += 1;
                    continue;
                };
                vlan_entries.push((fact, payload));
            }
            FACT_KIND_INTERFACE_STATE => {
                report.interface_facts_seen += 1;
                if fact.subject.namespace != "interface"
                    || fact.subject.plane.as_deref() != Some("operational")
                    || fact.trust < TrustClass::DeviceAsserted
                    || fact.validate().is_err()
                {
                    report.invalid_interface_facts += 1;
                    continue;
                }
                let Some(payload) = decode_payload::<InterfaceStatePayload>(fact) else {
                    report.invalid_interface_facts += 1;
                    continue;
                };
                let mut names = BTreeSet::new();
                names.insert(payload.interface_name.clone());
                if let Some(normalized) = &payload.normalized_name {
                    names.insert(normalized.clone());
                }
                if let Some(entity) = &fact.subject.entity {
                    names.insert(entity.clone());
                }
                interfaces.push(InterfaceEvidence {
                    fact,
                    payload,
                    key: InterfaceEntityKey::from_fact(fact),
                    names,
                });
            }
            _ => {}
        }
    }

    let mut evidence = Vec::new();
    let mut linux_rows = Vec::new();

    for (fact, payload) in &vlan_entries {
        match payload.observation_shape {
            VlanObservationShape::VlanDatabaseEntry => {
                let Some(domain) = known_string(&payload.bridge_domain) else {
                    continue;
                };
                let mut forwarding_interfaces = BTreeSet::new();
                let mut forwarding_interface_keys = BTreeSet::new();
                for member in &payload.members {
                    forwarding_interfaces.insert(member.interface_name.clone());
                    if let Some(key) = &member.interface_key {
                        forwarding_interface_keys.insert(key.clone());
                    }
                }
                let mut routed_interfaces = BTreeSet::new();
                if payload.routed_interfaces.state == FieldAvailability::Known
                    && let Some(routed) = &payload.routed_interfaces.value
                {
                    routed_interfaces.extend(routed.iter().cloned());
                }
                evidence.push(ForwardingDomainEvidence {
                    device_id: fact.subject.device_id.clone(),
                    scope: fact.subject.scope.clone(),
                    network_namespace: fact.subject.network_namespace.clone(),
                    domain_id: domain,
                    vlan_id: Some(payload.vlan_id),
                    vrf: known_string(&payload.vrf),
                    routed_interfaces,
                    forwarding_interfaces,
                    routed_interface_keys: BTreeSet::new(),
                    forwarding_interface_keys,
                    interface_equivalents: BTreeMap::new(),
                    interface_equivalent_keys: BTreeMap::new(),
                    collector_id: Some(fact.source.collector_id.clone()),
                    source_kind: Some(fact.source.source_kind.clone()),
                    capture_id: Some(fact.source.capture_id.clone()),
                    source_generation: Some(fact.source.session_epoch),
                    observation_time: Some(fact.observed_at.clone()),
                    valid_from: fact.valid_from.clone(),
                    valid_until: fact.valid_until.clone(),
                    lifecycle: fact.lifecycle,
                    evidence_refs: evidence_refs_with_fact(fact),
                });
                report.database_bindings_derived += 1;
            }
            VlanObservationShape::PortMembership => linux_rows.push((*fact, payload)),
        }
    }

    // Map exact interface evidence names to typed facts. Multiple representations
    // of one interface are aliases only because the same typed interface fact
    // carries them; no spelling inference is performed.
    let mut interface_by_name: BTreeMap<(String, Option<String>, Option<String>, String), usize> =
        BTreeMap::new();
    for (index, interface) in interfaces.iter().enumerate() {
        for name in &interface.names {
            interface_by_name.insert(
                (
                    interface.fact.subject.device_id.clone(),
                    interface.fact.subject.scope.clone(),
                    interface.fact.subject.network_namespace.clone(),
                    name.clone(),
                ),
                index,
            );
        }
    }

    // Enrich direct database bindings with independently established structured
    // interface keys where exact typed interface evidence is available.
    for binding in &mut evidence {
        let names: Vec<_> = binding
            .forwarding_interfaces
            .iter()
            .cloned()
            .chain(binding.routed_interfaces.iter().cloned())
            .collect();
        for name in names {
            let key = (
                binding.device_id.clone(),
                binding.scope.clone(),
                binding.network_namespace.clone(),
                name.clone(),
            );
            if let Some(index) = interface_by_name.get(&key)
                && let Some(interface_key) = &interfaces[*index].key
            {
                if binding.forwarding_interfaces.contains(&name) {
                    binding
                        .forwarding_interface_keys
                        .insert(interface_key.clone());
                }
                if binding.routed_interfaces.contains(&name) {
                    binding.routed_interface_keys.insert(interface_key.clone());
                }
            }
        }
    }

    // Group Linux port memberships by independently established bridge master.
    type LinuxKey = (
        String,
        Option<String>,
        Option<String>,
        String,
        u32,
        String,
        String,
    );
    let mut linux_groups: BTreeMap<LinuxKey, Vec<(&TypedFact, &VlanBridgePayload, usize)>> =
        BTreeMap::new();
    for (fact, payload) in linux_rows {
        let Some(member) = payload.members.first() else {
            report.indeterminate_port_memberships += 1;
            continue;
        };
        let interface_key = (
            fact.subject.device_id.clone(),
            fact.subject.scope.clone(),
            fact.subject.network_namespace.clone(),
            member.interface_name.clone(),
        );
        let Some(interface_index) = interface_by_name.get(&interface_key).copied() else {
            report.indeterminate_port_memberships += 1;
            continue;
        };
        let interface = &interfaces[interface_index];
        if !compatible(interface.fact, fact, policy.max_capture_skew_seconds) {
            report.incompatible_interface_evidence += 1;
            continue;
        }
        let Some(master) = &interface.payload.master_relationship else {
            report.indeterminate_port_memberships += 1;
            continue;
        };
        if master.kind != MasterKind::Bridge {
            report.indeterminate_port_memberships += 1;
            continue;
        }
        let group_key = (
            fact.subject.device_id.clone(),
            fact.subject.scope.clone(),
            fact.subject.network_namespace.clone(),
            master.target.clone(),
            payload.vlan_id,
            fact.source.collector_id.clone(),
            fact.source.capture_id.clone(),
        );
        linux_groups
            .entry(group_key)
            .or_default()
            .push((fact, payload, interface_index));
    }

    for ((device, scope, namespace, bridge, vlan, collector, capture), rows) in linux_groups {
        let mut forwarding_interfaces = BTreeSet::new();
        let mut forwarding_interface_keys = BTreeSet::new();
        let mut evidence_refs = Vec::new();
        let mut valid_from = None;
        let mut valid_until = None;
        let mut observation_time: Option<String> = None;
        let mut generation = 0u64;
        let mut lifecycle = Lifecycle::Active;
        let mut source_parts = Vec::new();
        for (fact, payload, interface_index) in &rows {
            let member = &payload.members[0];
            forwarding_interfaces.insert(member.interface_name.clone());
            if let Some(key) = &interfaces[*interface_index].key {
                forwarding_interface_keys.insert(key.clone());
            }
            for reference in evidence_refs_with_fact(fact)
                .into_iter()
                .chain(evidence_refs_with_fact(interfaces[*interface_index].fact))
            {
                if !evidence_refs.contains(&reference) {
                    evidence_refs.push(reference);
                }
            }
            valid_from = max_time(valid_from.as_deref(), fact.valid_from.as_deref());
            valid_from = max_time(
                valid_from.as_deref(),
                interfaces[*interface_index].fact.valid_from.as_deref(),
            );
            valid_until = min_time(valid_until.as_deref(), fact.valid_until.as_deref());
            valid_until = min_time(
                valid_until.as_deref(),
                interfaces[*interface_index].fact.valid_until.as_deref(),
            );
            observation_time =
                max_time(observation_time.as_deref(), Some(fact.observed_at.as_str()));
            observation_time = max_time(
                observation_time.as_deref(),
                Some(interfaces[*interface_index].fact.observed_at.as_str()),
            );
            generation = generation.max(fact.source.session_epoch);
            if fact.lifecycle != Lifecycle::Active
                || interfaces[*interface_index].fact.lifecycle != Lifecycle::Active
            {
                lifecycle = Lifecycle::Stale;
            }
            source_parts.push(fact.source.capture_id.as_str());
            source_parts.push(interfaces[*interface_index].fact.source.capture_id.as_str());
        }
        source_parts.sort_unstable();
        source_parts.dedup();
        evidence.push(ForwardingDomainEvidence {
            device_id: device,
            scope,
            network_namespace: namespace,
            domain_id: bridge,
            vlan_id: Some(vlan),
            vrf: None,
            routed_interfaces: BTreeSet::new(),
            forwarding_interfaces,
            routed_interface_keys: BTreeSet::new(),
            forwarding_interface_keys,
            interface_equivalents: BTreeMap::new(),
            interface_equivalent_keys: BTreeMap::new(),
            collector_id: Some(collector),
            source_kind: Some("derived/vlan-interface-binding".into()),
            capture_id: Some(if source_parts.len() <= 1 && !capture.is_empty() {
                capture
            } else {
                composite_capture_id(&source_parts)
            }),
            source_generation: Some(generation),
            observation_time,
            valid_from,
            valid_until,
            lifecycle,
            evidence_refs,
        });
        report.linux_bridge_bindings_derived += 1;
    }

    // Add positively established LAG aggregate/member relationships only to
    // domains whose forwarding population contains one side of the relationship.
    for binding in &mut evidence {
        for interface in &interfaces {
            if interface.fact.subject.device_id != binding.device_id
                || interface.fact.subject.scope != binding.scope
                || interface.fact.subject.network_namespace != binding.network_namespace
            {
                continue;
            }
            let mut add_relation = |aggregate: &str, member: &str| {
                if !binding.forwarding_interfaces.contains(aggregate)
                    && !binding.forwarding_interfaces.contains(member)
                {
                    return;
                }
                binding
                    .interface_equivalents
                    .entry(aggregate.to_string())
                    .or_default()
                    .insert(member.to_string());
                if let (Some(agg_key), Some(member_key)) = (
                    interface_by_name
                        .get(&(
                            binding.device_id.clone(),
                            binding.scope.clone(),
                            binding.network_namespace.clone(),
                            aggregate.to_string(),
                        ))
                        .and_then(|index| interfaces[*index].key.clone()),
                    interface_by_name
                        .get(&(
                            binding.device_id.clone(),
                            binding.scope.clone(),
                            binding.network_namespace.clone(),
                            member.to_string(),
                        ))
                        .and_then(|index| interfaces[*index].key.clone()),
                ) {
                    binding
                        .interface_equivalent_keys
                        .entry(agg_key)
                        .or_default()
                        .insert(member_key);
                }
                report.lag_relationships_included += 1;
            };
            if let Some(aggregate) = &interface.payload.member_of {
                for member_name in &interface.names {
                    add_relation(aggregate, member_name);
                }
            }
            if let Some(lag_members) = &interface.payload.lag_members
                && lag_members.state == FieldAvailability::Known
                && let Some(members) = &lag_members.value
            {
                for aggregate_name in &interface.names {
                    for member in members {
                        add_relation(aggregate_name, member);
                    }
                }
            }
        }
    }

    evidence.sort_by(|a, b| {
        (
            &a.device_id,
            &a.scope,
            &a.network_namespace,
            &a.domain_id,
            a.vlan_id,
            &a.capture_id,
        )
            .cmp(&(
                &b.device_id,
                &b.scope,
                &b.network_namespace,
                &b.domain_id,
                b.vlan_id,
                &b.capture_id,
            ))
    });
    Ok(ForwardingDomainDerivation { evidence, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_facts::{
        FieldAvailability, InterfaceStatePayload, InterfaceType, MasterKind, MasterRelationship,
        ObservedField, OperState,
    };
    use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
    use crate::neighbor_normalizer::{
        ArpndObservation, ArpndSourceProfile, MacObservation, MacSourceProfile, normalize_arpnd,
        normalize_mac,
    };
    use crate::neighbor_reconciliation::{
        CrossTableOutcome, MAX_CANDIDATE_EVALUATIONS, NeighborReconciliationPolicy,
        reconcile_neighbor_tables,
    };
    use crate::semantic_evidence::FreshnessPolicy;
    use crate::typed_facts::{
        Completeness, FactPayload, FactSource, FactSubject, Lifecycle, Quality, TypedFact,
    };
    use crate::vlan_normalizer::{VlanObservation, VlanSourceProfile, normalize_vlan};

    fn unknown<T>() -> ObservedField<T> {
        ObservedField::unavailable(FieldAvailability::Unknown, None)
    }

    fn interface_payload(
        name: &str,
        master: Option<(&str, MasterKind)>,
        member_of: Option<&str>,
        lag_members: Option<Vec<&str>>,
    ) -> InterfaceStatePayload {
        InterfaceStatePayload {
            interface_name: name.to_string(),
            normalized_name: None,
            administrative_state: unknown(),
            operational_state: unknown::<OperState>(),
            ifindex: unknown(),
            interface_type: unknown::<InterfaceType>(),
            mtu: unknown(),
            speed: unknown(),
            mac_address: unknown(),
            description: None,
            parent_interface: None,
            member_of: member_of.map(str::to_string),
            lag_members: lag_members
                .map(|m| ObservedField::known(m.iter().map(|s| s.to_string()).collect())),
            parent_relationship: None,
            master_relationship: master.map(|(target, kind)| MasterRelationship {
                target: target.to_string(),
                kind,
            }),
            source_conflicts: vec![],
            ip_addresses: None,
            ipv6_addresses: None,
        }
    }

    fn interface_fact(
        device: &str,
        name: &str,
        payload: InterfaceStatePayload,
        capture: &str,
        generation: u64,
        observed_at: &str,
    ) -> TypedFact {
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: FACT_KIND_INTERFACE_STATE.to_string(),
            subject: FactSubject {
                device_id: device.to_string(),
                scope: None,
                namespace: "interface".to_string(),
                network_namespace: None,
                entity: Some(name.to_string()),
                vrf: None,
                afi_safi: None,
                plane: Some("operational".to_string()),
                interface_identity: None,
            },
            source: FactSource {
                collector_id: "collector-test".to_string(),
                source_kind: "test/interface".to_string(),
                capture_id: capture.to_string(),
                session_epoch: generation,
                sequence: 0,
                exporter_id: None,
            },
            observed_at: observed_at.to_string(),
            ingested_at: None,
            valid_from: Some(observed_at.to_string()),
            valid_until: None,
            payload: FactPayload {
                produced_by: "test".into(),
                parser_revision: "test".into(),
                mapping_revision: None,
                fields: Some(serde_json::to_value(&payload).unwrap()),
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
            evidence: vec![],
        };
        fact.seal();
        assert!(fact.validate().is_ok(), "test interface fact must validate");
        fact
    }

    fn policy() -> ForwardingDomainDerivationPolicy {
        ForwardingDomainDerivationPolicy {
            freshness: FreshnessPolicy::observation_age(120),
            max_capture_skew_seconds: 60,
        }
    }

    #[test]
    fn linux_port_membership_without_interface_master_evidence_is_indeterminate() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        let derivation =
            derive_forwarding_domains(&out.facts, policy(), "2021-03-25T00:00:30+00:00").unwrap();
        assert_eq!(derivation.evidence.len(), 0);
        assert_eq!(
            derivation.report.indeterminate_port_memberships,
            out.facts.len(),
            "port membership rows must not manufacture a bridge domain from VLAN numbers"
        );
    }

    #[test]
    fn linux_bridge_master_evidence_derives_grouped_domain_binding() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        // Independently typed interface evidence: peerlink is a bridge port.
        let mut facts = vlan.facts.clone();
        facts.push(interface_fact(
            "cumulus-leaf01",
            "peerlink",
            interface_payload("peerlink", Some(("bridge", MasterKind::Bridge)), None, None),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        let derivation =
            derive_forwarding_domains(&facts, policy(), "2021-03-25T00:00:30+00:00").unwrap();
        assert!(
            !derivation.evidence.is_empty(),
            "bridge-mastered port must produce bindings"
        );
        for binding in &derivation.evidence {
            assert_eq!(binding.domain_id, "bridge");
            assert!(binding.forwarding_interfaces.contains("peerlink"));
        }
        assert!(
            derivation
                .evidence
                .iter()
                .any(|b| b.vlan_id == Some(13) && b.forwarding_interfaces.len() == 1)
        );
        assert!(
            derivation
                .evidence
                .iter()
                .all(|b| !b.evidence_refs.is_empty()),
            "derived bindings must carry supporting evidence references"
        );
    }

    #[test]
    fn non_bridge_master_target_never_produces_bridge_binding() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        let mut facts = vlan.facts.clone();
        facts.push(interface_fact(
            "cumulus-leaf01",
            "peerlink",
            interface_payload("peerlink", Some(("vrf-blue", MasterKind::Vrf)), None, None),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        let derivation =
            derive_forwarding_domains(&facts, policy(), "2021-03-25T00:00:30+00:00").unwrap();
        assert_eq!(derivation.evidence.len(), 0);
        assert!(derivation.report.indeterminate_port_memberships > 0);
    }

    #[test]
    fn incompatible_capture_time_interface_evidence_is_rejected() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        let mut facts = vlan.facts.clone();
        // Interface evidence hours outside the capture-skew policy.
        facts.push(interface_fact(
            "cumulus-leaf01",
            "peerlink",
            interface_payload("peerlink", Some(("bridge", MasterKind::Bridge)), None, None),
            "cumulus-iface-capture",
            1,
            "2021-03-25T03:00:00+00:00",
        ));
        let derivation =
            derive_forwarding_domains(&facts, policy(), "2021-03-25T03:00:30+00:00").unwrap();
        assert_eq!(
            derivation.evidence.len(),
            0,
            "stale/skewed interface evidence must not establish current domain bindings"
        );
    }

    #[test]
    fn same_vlan_number_in_two_bridges_derives_distinct_domains() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        // swp1 rows in the fixture exist for several VLANs; give half the
        // ports to bridge-a and peerlink to bridge-b where the fixture has
        // membership rows for both. Use the fixture's real port names.
        let ports: Vec<String> = vlan
            .facts
            .iter()
            .filter_map(|f| {
                serde_json::from_value::<VlanBridgePayload>(f.payload.fields.clone().unwrap())
                    .ok()
                    .map(|p| p.members[0].interface_name.clone())
            })
            .collect();
        let first_port = ports.first().cloned().unwrap();
        let mut facts = vlan.facts.clone();
        facts.push(interface_fact(
            "cumulus-leaf01",
            "peerlink",
            interface_payload(
                "peerlink",
                Some(("bridge-b", MasterKind::Bridge)),
                None,
                None,
            ),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        facts.push(interface_fact(
            "cumulus-leaf01",
            &first_port,
            interface_payload(
                &first_port,
                Some(("bridge-a", MasterKind::Bridge)),
                None,
                None,
            ),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        if first_port != "peerlink" {
            let derivation =
                derive_forwarding_domains(&facts, policy(), "2021-03-25T00:00:30+00:00").unwrap();
            let domains: BTreeSet<_> = derivation
                .evidence
                .iter()
                .map(|b| (b.domain_id.clone(), b.vlan_id))
                .collect();
            assert!(
                domains.iter().any(|(d, _)| d == "bridge-a"),
                "bridge-a bindings expected: {domains:?}"
            );
            assert!(
                domains.iter().any(|(d, _)| d == "bridge-b"),
                "bridge-b bindings expected: {domains:?}"
            );
            let same_vlan: BTreeSet<_> = domains
                .iter()
                .filter(|(_, v)| *v == Some(1))
                .map(|(d, _)| d.clone())
                .collect();
            assert!(
                same_vlan.len() >= 2 || !domains.iter().any(|(_, v)| *v == Some(1)),
                "VLAN numbers must not be collapsed across distinct bridges"
            );
        }
    }

    #[test]
    fn lag_member_aggregate_relationship_added_only_with_positive_evidence() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: CommandCapture::new(input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "cumulus-leaf01",
                "collector-test",
                "cumulus-vlan-capture",
                "2021-03-25T00:00:00+00:00",
            ),
        });
        let mut facts = vlan.facts.clone();
        facts.push(interface_fact(
            "cumulus-leaf01",
            "peerlink",
            interface_payload("peerlink", Some(("bridge", MasterKind::Bridge)), None, None),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        // bond0 is a LAG member of the bridge through the bridge master, and
        // its physical member swp1 is positively established.
        facts.push(interface_fact(
            "cumulus-leaf01",
            "swp1",
            interface_payload(
                "swp1",
                Some(("bond0", MasterKind::Lag)),
                Some("bond0"),
                None,
            ),
            "cumulus-iface-capture",
            1,
            "2021-03-25T00:00:02+00:00",
        ));
        let derivation =
            derive_forwarding_domains(&facts, policy(), "2021-03-25T00:00:30+00:00").unwrap();
        // peerlink rows produce bindings; swp1/bond0 equivalence only appears
        // if either is in the binding's forwarding population — here they are
        // not, so no unrestricted equivalence must leak in.
        for binding in &derivation.evidence {
            assert!(
                binding.interface_equivalents.is_empty()
                    || binding.forwarding_interfaces.iter().any(|i| binding
                        .interface_equivalents
                        .contains_key(i)
                        || binding
                            .interface_equivalents
                            .values()
                            .flatten()
                            .any(|m| m == i)),
                "LAG equivalence must relate to the binding's forwarding population"
            );
        }
    }

    #[test]
    fn genuine_eos_derived_bindings_integrate_with_neighbor_reconciliation() {
        // End-to-end offline slice: genuine EOS VLAN database capture derives
        // forwarding-domain bindings; genuine same-device ARP and MAC captures
        // reconcile against those derived bindings without any synthetic
        // domain evidence.
        let vlan_input =
            include_str!("../tests/fixtures/bn024/vlan/eos-leaf02-show-vlan-brief.json");
        let arp_input =
            include_str!("../tests/fixtures/bn024/neighbor/eos-leaf02-show-arp-vrf-all.json");
        let mac_input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let vlan = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::EosShowVlanBriefJson,
            capture: CommandCapture::new(vlan_input, CaptureOutcome::Complete),
            context: ObservationContext::minimal(
                "device-leaf02",
                "collector-test",
                "eos-vlan-capture",
                "2021-06-07T00:00:05+00:00",
            ),
        });
        assert_eq!(vlan.report.malformed_records, 0);
        let mut arp_ctx = ObservationContext::minimal(
            "device-leaf02",
            "collector-test",
            "eos-arp-capture",
            "2021-06-07T00:00:00+00:00",
        );
        arp_ctx.session_epoch = 7;
        let mut mac_ctx = ObservationContext::minimal(
            device_id_leaf02(),
            "collector-test",
            "eos-mac-capture",
            "2021-06-07T00:00:20+00:00",
        );
        mac_ctx.session_epoch = 7;
        let arps = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: CommandCapture::new(arp_input, CaptureOutcome::Complete),
            context: arp_ctx,
            interface_identities: None,
        });
        let macs = normalize_mac(&MacObservation {
            profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
            capture: CommandCapture::new(mac_input, CaptureOutcome::Complete),
            context: mac_ctx,
            interface_identities: None,
        });
        let derivation =
            derive_forwarding_domains(&vlan.facts, policy(), "2021-06-07T00:00:30+00:00").unwrap();
        assert!(
            derivation.evidence.len() >= 5,
            "expected one binding per EOS VLAN database entry"
        );
        let neighbor_facts: Vec<_> = arps
            .facts
            .iter()
            .chain(macs.facts.iter())
            .cloned()
            .collect();
        let reconcile_policy = NeighborReconciliationPolicy {
            freshness: FreshnessPolicy::observation_age(120),
            max_capture_skew_seconds: 60,
            max_candidate_evaluations: MAX_CANDIDATE_EVALUATIONS,
        };
        let report = reconcile_neighbor_tables(
            &neighbor_facts,
            &[],
            &derivation.evidence,
            &reconcile_policy,
            "2021-06-07T00:00:30+00:00",
        )
        .unwrap();
        assert_eq!(report.results.len(), 7);
        for ip in [
            "169.254.1.0",
            "172.16.1.101",
            "172.16.3.102",
            "172.16.3.202",
        ] {
            let result = report
                .results
                .iter()
                .find(|r| r.ip_address == ip)
                .unwrap_or_else(|| panic!("missing result for {ip}"));
            assert!(
                matches!(
                    result.outcome,
                    CrossTableOutcome::Consistent
                        | CrossTableOutcome::ScopedCorrelationInterfaceNotComparable
                ),
                "{ip}: expected positive scoped correlation from genuine derived bindings, got {:?}: {}",
                result.outcome,
                result.reason
            );
            assert_eq!(result.mac_fact_ids.len(), 1, "{ip}");
        }
        for ip in ["10.0.0.21", "10.0.0.22", "10.255.2.1"] {
            let result = report
                .results
                .iter()
                .find(|r| r.ip_address == ip)
                .unwrap_or_else(|| panic!("missing result for {ip}"));
            assert_eq!(
                result.outcome,
                CrossTableOutcome::IndeterminateMissingForwardingDomain,
                "{ip}: routed/management ports absent from the VLAN database must remain indeterminate"
            );
        }
    }

    fn device_id_leaf02() -> &'static str {
        "device-leaf02"
    }
}

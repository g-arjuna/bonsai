//! BN-024-06 first capability-led relationship assessment slice.
//!
//! Consumes typed `interface.state` facts and answers bounded operational
//! questions about LAG, bridge, parent/master and VRF evidence without
//! inventing missing relationships. This is deliberately a pure analysis
//! module: it performs no graph writes, no acquisition and no device access.
//! Operator/API exposure is deferred to the BN-030 interface workspace work.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::interface_facts::{
    FACT_KIND_INTERFACE_STATE, InterfaceStatePayload, InterfaceType, MasterKind, OperState,
};
use crate::typed_facts::TypedFact;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentVerdict {
    Pass,
    Fail,
    Unknown,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LagMemberAssessment {
    pub interface: String,
    pub operational_state: Option<String>,
    pub fact_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LagAssessment {
    pub aggregate: String,
    pub aggregate_present: bool,
    pub aggregate_operational_state: Option<String>,
    pub member_count: usize,
    pub oper_up_members: usize,
    pub oper_down_members: usize,
    pub unknown_members: usize,
    pub members: Vec<LagMemberAssessment>,
    pub verdict: AssessmentVerdict,
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipEdge {
    pub interface: String,
    pub target: String,
    pub kind: String,
    pub fact_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VrfAttachmentEvidence {
    pub interface: String,
    pub network_instance: String,
    pub evidence_axis: String,
    pub fact_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeRelationshipReport {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    pub interface_count: usize,
    pub verdict: AssessmentVerdict,
    pub lag_assessments: Vec<LagAssessment>,
    pub bridge_memberships: Vec<RelationshipEdge>,
    pub parent_relationships: Vec<RelationshipEdge>,
    pub vrf_attachments: Vec<VrfAttachmentEvidence>,
    pub unresolved_relationships: Vec<RelationshipEdge>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceRelationshipReport {
    pub scopes: Vec<ScopeRelationshipReport>,
    /// Valid-shaped facts that could not contribute because the payload was
    /// absent or not decodable as `interface.state`. Missing evidence is
    /// explicit rather than silently treated as absence.
    pub rejected_inputs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey<'a> {
    device_id: &'a str,
    scope: Option<&'a str>,
    network_namespace: Option<&'a str>,
}

struct InterfaceRow<'a> {
    entity: String,
    payload: InterfaceStatePayload,
    fact: &'a TypedFact,
}

fn oper_name(value: Option<OperState>) -> Option<String> {
    value.map(|v| format!("{v:?}").to_ascii_lowercase())
}

fn relationship_kind(kind: MasterKind) -> &'static str {
    match kind {
        MasterKind::Lag => "lag",
        MasterKind::Bridge => "bridge",
        MasterKind::Vrf => "vrf",
        MasterKind::Parent => "parent",
        MasterKind::Unresolved => "unresolved",
    }
}

fn edge(interface: &str, target: &str, kind: &str, fact: &TypedFact) -> RelationshipEdge {
    RelationshipEdge {
        interface: interface.into(),
        target: target.into(),
        kind: kind.into(),
        fact_id: fact.fact_id.clone(),
    }
}

/// Analyze a scoped set of typed interface facts. The caller owns snapshot
/// selection; this function does not mix lifetimes silently — every fact in
/// the input is treated as one caller-selected snapshot and grouped only by
/// explicit subject identity axes.
pub fn analyze_interface_relationships(facts: &[TypedFact]) -> InterfaceRelationshipReport {
    let mut grouped: BTreeMap<ScopeKey<'_>, Vec<InterfaceRow<'_>>> = BTreeMap::new();
    let mut rejected_inputs = Vec::new();

    for fact in facts {
        if fact.fact_kind != FACT_KIND_INTERFACE_STATE {
            continue;
        }
        if let Err(err) = fact.validate() {
            rejected_inputs.push(format!("{}: typed fact invalid ({err:?})", fact.fact_id));
            continue;
        }
        let Some(fields) = &fact.payload.fields else {
            rejected_inputs.push(format!("{}: payload fields absent", fact.fact_id));
            continue;
        };
        let payload: InterfaceStatePayload = match serde_json::from_value(fields.clone()) {
            Ok(payload) => payload,
            Err(_) => {
                rejected_inputs.push(format!("{}: interface payload undecodable", fact.fact_id));
                continue;
            }
        };
        let Some(entity) = fact.subject.entity.clone() else {
            rejected_inputs.push(format!("{}: canonical subject entity absent", fact.fact_id));
            continue;
        };
        grouped
            .entry(ScopeKey {
                device_id: fact.subject.device_id.as_str(),
                scope: fact.subject.scope.as_deref(),
                network_namespace: fact.subject.network_namespace.as_deref(),
            })
            .or_default()
            .push(InterfaceRow {
                entity,
                payload,
                fact,
            });
    }

    let mut scopes = Vec::new();
    for (key, rows) in grouped {
        let known_names: BTreeSet<&str> = rows.iter().map(|r| r.entity.as_str()).collect();
        let mut members_by_lag: BTreeMap<String, Vec<&InterfaceRow<'_>>> = BTreeMap::new();
        let mut bridge_memberships = Vec::new();
        let mut parent_relationships = Vec::new();
        let mut vrf_attachments = Vec::new();
        let mut unresolved_relationships = Vec::new();
        let mut seen_vrf = BTreeSet::new();

        for row in &rows {
            if let Some(lag) = &row.payload.member_of {
                members_by_lag.entry(lag.clone()).or_default().push(row);
            }
            if let Some(master) = &row.payload.master_relationship {
                let e = edge(
                    &row.entity,
                    &master.target,
                    relationship_kind(master.kind),
                    row.fact,
                );
                match master.kind {
                    MasterKind::Bridge => bridge_memberships.push(e),
                    MasterKind::Parent => parent_relationships.push(e),
                    MasterKind::Vrf => {
                        if seen_vrf.insert((row.entity.clone(), master.target.clone(), "master")) {
                            vrf_attachments.push(VrfAttachmentEvidence {
                                interface: row.entity.clone(),
                                network_instance: master.target.clone(),
                                evidence_axis: "master_relationship".into(),
                                fact_id: row.fact.fact_id.clone(),
                            });
                        }
                    }
                    MasterKind::Unresolved => unresolved_relationships.push(e),
                    MasterKind::Lag => {}
                }
                if master.kind != MasterKind::Unresolved
                    && !known_names.contains(master.target.as_str())
                {
                    unresolved_relationships.push(edge(
                        &row.entity,
                        &master.target,
                        "missing_target",
                        row.fact,
                    ));
                }
            }
            if let Some(parent) = &row.payload.parent_relationship {
                parent_relationships.push(edge(
                    &row.entity,
                    &parent.target,
                    relationship_kind(parent.kind),
                    row.fact,
                ));
                if !known_names.contains(parent.target.as_str()) {
                    unresolved_relationships.push(edge(
                        &row.entity,
                        &parent.target,
                        "missing_parent_target",
                        row.fact,
                    ));
                }
            }
            if let Some(vrf) = &row.fact.subject.vrf
                && seen_vrf.insert((row.entity.clone(), vrf.clone(), "subject"))
            {
                vrf_attachments.push(VrfAttachmentEvidence {
                    interface: row.entity.clone(),
                    network_instance: vrf.clone(),
                    evidence_axis: "subject.vrf".into(),
                    fact_id: row.fact.fact_id.clone(),
                });
            }
        }

        let mut lag_assessments = Vec::new();
        for (aggregate, members) in members_by_lag {
            let aggregate_row = rows.iter().find(|r| r.entity == aggregate);
            let aggregate_present = aggregate_row.is_some_and(|r| {
                r.payload.interface_type.value == Some(InterfaceType::PortChannel)
            });
            let aggregate_oper =
                aggregate_row.and_then(|r| oper_name(r.payload.operational_state.value));
            let mut member_rows = Vec::new();
            let mut up = 0usize;
            let mut down = 0usize;
            let mut unknown = 0usize;
            for member in members {
                let state = member.payload.operational_state.value;
                match state {
                    Some(OperState::Up) => up += 1,
                    Some(OperState::Down)
                    | Some(OperState::LowerLayerDown)
                    | Some(OperState::Dormant) => down += 1,
                    _ => unknown += 1,
                }
                member_rows.push(LagMemberAssessment {
                    interface: member.entity.clone(),
                    operational_state: oper_name(state),
                    fact_id: member.fact.fact_id.clone(),
                });
            }
            member_rows.sort_by(|a, b| a.interface.cmp(&b.interface));
            let member_count = member_rows.len();
            let mut findings = Vec::new();
            let mut verdict = AssessmentVerdict::Pass;
            if !aggregate_present {
                verdict = AssessmentVerdict::Unknown;
                findings.push("aggregate_missing_or_not_independently_typed_as_lag".into());
            }
            if unknown > 0 {
                verdict = AssessmentVerdict::Unknown;
                findings.push("member_operational_state_unknown".into());
            }
            if down > 0 {
                verdict = AssessmentVerdict::Fail;
                findings.push("lag_member_down".into());
                if up == 0 {
                    findings.push("lag_no_active_members".into());
                } else {
                    findings.push("lag_reduced_capacity".into());
                }
            }
            if aggregate_oper.as_deref() == Some("down") && up > 0 {
                verdict = AssessmentVerdict::Fail;
                findings.push("lag_aggregate_down_while_member_up".into());
            }
            if member_count == 0 {
                verdict = AssessmentVerdict::Unknown;
                findings.push("lag_membership_unestablished".into());
            }
            lag_assessments.push(LagAssessment {
                aggregate,
                aggregate_present,
                aggregate_operational_state: aggregate_oper,
                member_count,
                oper_up_members: up,
                oper_down_members: down,
                unknown_members: unknown,
                members: member_rows,
                verdict,
                findings,
            });
        }
        lag_assessments.sort_by(|a, b| a.aggregate.cmp(&b.aggregate));

        let verdict = if lag_assessments.is_empty() {
            AssessmentVerdict::NotApplicable
        } else if lag_assessments
            .iter()
            .any(|l| l.verdict == AssessmentVerdict::Fail)
        {
            AssessmentVerdict::Fail
        } else if lag_assessments
            .iter()
            .any(|l| l.verdict == AssessmentVerdict::Unknown)
            || !unresolved_relationships.is_empty()
        {
            AssessmentVerdict::Unknown
        } else {
            AssessmentVerdict::Pass
        };

        scopes.push(ScopeRelationshipReport {
            device_id: key.device_id.into(),
            scope: key.scope.map(str::to_string),
            network_namespace: key.network_namespace.map(str::to_string),
            interface_count: known_names.len(),
            verdict,
            lag_assessments,
            bridge_memberships,
            parent_relationships,
            vrf_attachments,
            unresolved_relationships,
        });
    }

    InterfaceRelationshipReport {
        scopes,
        rejected_inputs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_normalizer::{
        CaptureOutcome, CommandCapture, InterfaceObservation, NormalizerNos, ObservationContext,
        normalize,
    };

    fn ctx<'a>() -> ObservationContext<'a> {
        ObservationContext {
            device_id: "leaf01",
            scope: None,
            network_namespace: Some("host"),
            collector_id: "collector-a",
            capture_id: "cap-iface-genuine",
            session_epoch: 1,
            sequence: 7,
            observed_at: "2026-11-24T10:00:00Z",
            release_raw: None,
            platform: None,
            release_train: None,
            release_build: None,
            release_components: std::collections::BTreeMap::new(),
            platform_sku: None,
            asic: None,
            hardware_attributes: std::collections::BTreeMap::new(),
        }
    }

    fn genuine_cumulus_facts() -> Vec<TypedFact> {
        // Genuine upstream-derived committed fixture pair. This is not a
        // synthetic expected-value claim: the expected relationships below
        // are independently visible in the iproute2 master/link lines.
        let vtysh =
            include_str!("../tests/fixtures/bn024/interface-cumulus/vtysh-show-int-vrf-all.txt");
        let ip = include_str!("../tests/fixtures/bn024/interface-cumulus/ip-d-address-show.txt");
        normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh: CommandCapture::new(vtysh, CaptureOutcome::Complete),
            ip_addr: CommandCapture::new(ip, CaptureOutcome::Complete),
            context: ctx(),
        })
        .facts
    }

    #[test]
    fn genuine_cumulus_lag_bridge_and_vrf_relationships_are_reported() {
        let facts = genuine_cumulus_facts();
        let report = analyze_interface_relationships(&facts);
        assert!(report.rejected_inputs.is_empty());
        assert_eq!(report.scopes.len(), 1);
        let scope = &report.scopes[0];
        assert!(scope.interface_count > 20);

        // bond01 is visible as a raw master target for swp1, but the
        // genuine capture does not independently establish bond01's type
        // in the iproute2 half. The normalizer therefore preserves the
        // relationship as unresolved rather than inferring LAG membership.
        assert!(
            !scope
                .lag_assessments
                .iter()
                .any(|l| l.aggregate == "bond01")
        );
        assert!(
            scope
                .unresolved_relationships
                .iter()
                .any(|e| e.interface == "swp1" && e.target == "bond01")
        );

        let peerlink = scope
            .lag_assessments
            .iter()
            .find(|l| l.aggregate == "peerlink")
            .expect("peerlink assessment");
        assert_eq!(peerlink.member_count, 2);
        assert_eq!(peerlink.oper_up_members, 2);
        assert_eq!(peerlink.verdict, AssessmentVerdict::Pass);

        assert!(
            scope
                .bridge_memberships
                .iter()
                .any(|e| e.interface == "bond02" && e.target == "bridge")
        );
        assert!(
            scope
                .vrf_attachments
                .iter()
                .any(|v| v.interface == "vlan13" && v.network_instance == "evpn-vrf")
        );
    }

    #[test]
    fn failed_member_produces_reduced_capacity_fail() {
        let mut facts = genuine_cumulus_facts();
        let swp49 = facts
            .iter_mut()
            .find(|f| f.subject.entity.as_deref() == Some("swp49"))
            .expect("swp49 fact");
        let mut payload: InterfaceStatePayload =
            serde_json::from_value(swp49.payload.fields.clone().unwrap()).unwrap();
        payload.operational_state = crate::interface_facts::ObservedField::known(OperState::Down);
        swp49.payload.fields = Some(serde_json::to_value(payload).unwrap());
        swp49.seal();

        let report = analyze_interface_relationships(&facts);
        let scope = &report.scopes[0];
        let peerlink = scope
            .lag_assessments
            .iter()
            .find(|l| l.aggregate == "peerlink")
            .unwrap();
        assert_eq!(peerlink.verdict, AssessmentVerdict::Fail);
        assert!(peerlink.findings.contains(&"lag_member_down".to_string()));
        assert!(
            peerlink
                .findings
                .contains(&"lag_reduced_capacity".to_string())
        );
        assert!(
            !peerlink
                .findings
                .contains(&"lag_no_active_members".to_string())
        );
        assert_eq!(scope.verdict, AssessmentVerdict::Fail);
    }
}

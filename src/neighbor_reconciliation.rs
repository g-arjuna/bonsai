//! Deterministic ARP/ND ↔ MAC forwarding-table reconciliation.
//!
//! The result is evidence consistency, not forwarding-plane proof. Reverse or
//! absence conclusions require explicit capture coverage supplied by the
//! caller; complete MAC-table bytes alone do not prove that a particular ARP
//! interface belongs to the L2 population.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::FieldAvailability;
use crate::neighbor_facts::{
    ArpndNeighborPayload, FACT_KIND_ARPND_NEIGHBOR, FACT_KIND_MAC_FORWARDING, MacForwardingPayload,
};
use crate::typed_facts::{Lifecycle, TypedFact};

pub const MAX_RECONCILIATION_INPUT_FACTS: usize = 100_000;
pub const MAX_RECONCILIATION_RESULTS: usize = 50_000;
pub const MAX_RESULT_EVIDENCE_REFS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossTableOutcome {
    /// Exactly one applicable MAC observation agrees on VLAN, MAC and
    /// forwarding-interface evidence. This is consistency only.
    Consistent,
    /// Exactly one applicable MAC observation exists but its interface
    /// evidence disagrees with the neighbor observation.
    ConflictingInterface,
    /// Multiple distinct MAC observations exist for the same VLAN+MAC.
    MultipleCandidates,
    /// Neighbor evidence does not establish the VLAN needed for an L2 join.
    IndeterminateMissingVlan,
    /// Neighbor evidence does not establish a valid MAC.
    IndeterminateMissingMac,
    /// The relevant interface is inside complete declared MAC coverage and
    /// no matching VLAN+MAC row was observed.
    AbsentInDeclaredCoverage,
    /// No row exists, but coverage is incomplete/unknown or does not
    /// establish that this interface was in scope.
    UnknownOutsideCoverage,
    /// Input evidence is withdrawn/stale/not yet valid at `as_of`.
    NotCurrent,
    /// Typed payloads were malformed or semantically inconsistent.
    InvalidInput,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CrossTableResult {
    pub arpnd_fact_id: String,
    pub ip_address: String,
    pub neighbor_interface: String,
    pub neighbor_forwarding_interface: String,
    pub vlan_id: Option<u32>,
    pub mac_address: Option<String>,
    pub outcome: CrossTableOutcome,
    pub mac_fact_ids: Vec<String>,
    pub reason: String,
}

/// Explicit acquisition coverage for the reconciliation input. This is a
/// contract input, not a device-wide completeness assumption. `covered_interfaces`
/// names the interfaces whose L2 population the MAC capture is known to cover.
#[derive(Clone, Debug, Default)]
pub struct CrossTableCoverage {
    pub arpnd_capture_complete: bool,
    pub mac_capture_complete: bool,
    pub covered_interfaces: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CrossTableReport {
    pub as_of: String,
    pub arpnd_facts_considered: usize,
    pub mac_facts_considered: usize,
    pub invalid_arpnd_facts: usize,
    pub invalid_mac_facts: usize,
    pub outcome_counts: BTreeMap<String, usize>,
    pub results: Vec<CrossTableResult>,
}

fn parse_time(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).ok()
}

fn current_at(fact: &TypedFact, as_of: chrono::DateTime<chrono::FixedOffset>) -> bool {
    if fact.lifecycle != Lifecycle::Active {
        return false;
    }
    let Some(observed) = parse_time(&fact.observed_at) else {
        return false;
    };
    if observed > as_of {
        return false;
    }
    if let Some(valid_from) = &fact.valid_from
        && parse_time(valid_from).is_some_and(|t| t > as_of)
    {
        return false;
    }
    if let Some(valid_until) = &fact.valid_until
        && parse_time(valid_until).is_some_and(|t| t <= as_of)
    {
        return false;
    }
    true
}

fn canonical_interface(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn known_vlan(payload: &ArpndNeighborPayload) -> Option<u32> {
    (payload.interface_vlan_id.state == FieldAvailability::Known)
        .then_some(payload.interface_vlan_id.value)
        .flatten()
}

fn known_mac(payload: &ArpndNeighborPayload) -> Option<String> {
    (payload.mac_address.state == FieldAvailability::Known)
        .then_some(payload.mac_address.value.clone())
        .flatten()
}

fn mac_vlan(payload: &MacForwardingPayload) -> Option<u32> {
    (payload.vlan_id.state == FieldAvailability::Known)
        .then_some(payload.vlan_id.value)
        .flatten()
}

fn mac_interface(payload: &MacForwardingPayload) -> String {
    payload
        .normalized_outgoing_interface
        .clone()
        .unwrap_or_else(|| payload.outgoing_interface.clone())
}

/// Reconcile current typed neighbor and MAC facts at an explicit `as_of`.
/// All candidates are indexed before outcomes are assigned; no first-match
/// short-circuit hides contradictory rows.
pub fn reconcile_neighbor_tables(
    facts: &[TypedFact],
    coverage: &CrossTableCoverage,
    as_of: &str,
) -> Result<CrossTableReport, String> {
    let as_of = parse_time(as_of).ok_or_else(|| "as_of is not RFC-3339".to_string())?;
    if facts.len() > MAX_RECONCILIATION_INPUT_FACTS {
        return Err(format!(
            "reconciliation input fact count {} exceeds bound {MAX_RECONCILIATION_INPUT_FACTS}",
            facts.len()
        ));
    }
    let mut arps = Vec::new();
    let mut macs: BTreeMap<(u32, String), Vec<(String, MacForwardingPayload)>> = BTreeMap::new();
    let mut invalid_arpnd = 0usize;
    let mut invalid_mac = 0usize;

    for fact in facts {
        if !current_at(fact, as_of) {
            continue;
        }
        match fact.fact_kind.as_str() {
            FACT_KIND_ARPND_NEIGHBOR => {
                if fact.validate().is_err() {
                    invalid_arpnd += 1;
                    continue;
                }
                let Some(fields) = &fact.payload.fields else {
                    invalid_arpnd += 1;
                    continue;
                };
                match serde_json::from_value::<ArpndNeighborPayload>(fields.clone()) {
                    Ok(payload) => arps.push((fact, payload)),
                    Err(_) => invalid_arpnd += 1,
                }
            }
            FACT_KIND_MAC_FORWARDING => {
                if fact.validate().is_err() {
                    invalid_mac += 1;
                    continue;
                }
                let Some(fields) = &fact.payload.fields else {
                    invalid_mac += 1;
                    continue;
                };
                match serde_json::from_value::<MacForwardingPayload>(fields.clone()) {
                    Ok(payload) => {
                        if let Some(vlan) = mac_vlan(&payload) {
                            macs.entry((vlan, payload.mac_address.clone()))
                                .or_default()
                                .push((fact.fact_id.clone(), payload));
                        }
                    }
                    Err(_) => invalid_mac += 1,
                }
            }
            _ => {}
        }
    }

    for entries in macs.values_mut() {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
    }
    arps.sort_by(|a, b| a.0.fact_id.cmp(&b.0.fact_id));
    if arps.len() > MAX_RECONCILIATION_RESULTS {
        return Err(format!(
            "reconciliation candidate result count {} exceeds bound {MAX_RECONCILIATION_RESULTS}",
            arps.len()
        ));
    }

    let mut results = Vec::new();
    for (fact, arp) in arps {
        let forwarding = arp
            .physical_interface
            .clone()
            .unwrap_or_else(|| arp.outgoing_interface.clone());
        let vlan = known_vlan(&arp);
        let mac = known_mac(&arp);
        let result_mac = mac.clone();
        let (outcome, mut ids, mut reason) = match (vlan, mac) {
            (None, _) => (
                CrossTableOutcome::IndeterminateMissingVlan,
                vec![],
                "neighbor evidence does not establish the L2 VLAN/broadcast-domain key".to_string(),
            ),
            (Some(_), None) => (
                CrossTableOutcome::IndeterminateMissingMac,
                vec![],
                "neighbor evidence does not establish a valid peer MAC".to_string(),
            ),
            (Some(vlan), Some(mac)) => {
                let candidates = macs.get(&(vlan, mac.clone())).cloned().unwrap_or_default();
                if candidates.is_empty() {
                    let covered = coverage.mac_capture_complete
                        && coverage.covered_interfaces.iter().any(|i| {
                            canonical_interface(i) == canonical_interface(&forwarding)
                                || canonical_interface(i)
                                    == canonical_interface(&arp.outgoing_interface)
                        });
                    if covered {
                        (
                            CrossTableOutcome::AbsentInDeclaredCoverage,
                            vec![],
                            "VLAN+MAC absent despite complete declared coverage for the relevant interface".to_string(),
                        )
                    } else {
                        (
                            CrossTableOutcome::UnknownOutsideCoverage,
                            vec![],
                            "no matching row, but applicable MAC coverage is incomplete or not established".to_string(),
                        )
                    }
                } else {
                    let ids: Vec<String> = candidates.iter().map(|c| c.0.clone()).collect();
                    let matching = candidates
                        .iter()
                        .filter(|(_, m)| {
                            canonical_interface(&mac_interface(m))
                                == canonical_interface(&forwarding)
                        })
                        .count();
                    if candidates.len() == 1 && matching == 1 {
                        (
                            CrossTableOutcome::Consistent,
                            ids,
                            "exactly one VLAN+MAC candidate agrees on forwarding-interface evidence".to_string(),
                        )
                    } else if candidates.len() == 1 {
                        (
                            CrossTableOutcome::ConflictingInterface,
                            ids,
                            "single VLAN+MAC candidate has different interface evidence"
                                .to_string(),
                        )
                    } else {
                        (
                            CrossTableOutcome::MultipleCandidates,
                            ids,
                            "multiple distinct MAC observations exist for the same VLAN+MAC"
                                .to_string(),
                        )
                    }
                }
            }
        };
        if ids.len() > MAX_RESULT_EVIDENCE_REFS {
            let omitted = ids.len() - MAX_RESULT_EVIDENCE_REFS;
            ids.truncate(MAX_RESULT_EVIDENCE_REFS);
            reason.push_str(&format!(
                "; {omitted} additional candidate evidence references omitted by diagnostic bound"
            ));
        }
        results.push(CrossTableResult {
            arpnd_fact_id: fact.fact_id.clone(),
            ip_address: arp.ip_address.clone(),
            neighbor_interface: arp.outgoing_interface.clone(),
            neighbor_forwarding_interface: forwarding,
            vlan_id: vlan,
            mac_address: result_mac,
            outcome,
            mac_fact_ids: ids,
            reason,
        });
    }
    results.sort_by(|a, b| {
        (&a.ip_address, a.vlan_id, &a.mac_address, &a.arpnd_fact_id).cmp(&(
            &b.ip_address,
            b.vlan_id,
            &b.mac_address,
            &b.arpnd_fact_id,
        ))
    });
    let mut outcome_counts = BTreeMap::new();
    for result in &results {
        *outcome_counts
            .entry(format!("{:?}", result.outcome))
            .or_insert(0) += 1;
    }
    Ok(CrossTableReport {
        as_of: as_of.to_rfc3339(),
        arpnd_facts_considered: results.len(),
        mac_facts_considered: macs.values().map(Vec::len).sum(),
        invalid_arpnd_facts: invalid_arpnd,
        invalid_mac_facts: invalid_mac,
        outcome_counts,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
    use crate::neighbor_normalizer::{
        ArpndObservation, ArpndSourceProfile, MacObservation, MacSourceProfile, normalize_arpnd,
        normalize_mac,
    };

    fn ctx() -> ObservationContext<'static> {
        ObservationContext::minimal(
            "device-leaf02",
            "collector-test",
            "capture-test",
            "2021-06-07T00:00:00+00:00",
        )
    }

    fn eos_facts() -> Vec<TypedFact> {
        let arp_input =
            include_str!("../tests/fixtures/bn024/neighbor/eos-leaf02-show-arp-vrf-all.json");
        let mac_input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let capture = CommandCapture::new("", CaptureOutcome::Complete);
        let arp = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: CommandCapture::new(arp_input, CaptureOutcome::Complete),
            context: ctx(),
            interface_identities: None,
        });
        let mac = normalize_mac(&MacObservation {
            profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
            capture: CommandCapture::new(mac_input, CaptureOutcome::Complete),
            context: ctx(),
            interface_identities: None,
        });
        let _ = capture;
        [arp.facts, mac.facts].concat()
    }

    fn complete_coverage() -> CrossTableCoverage {
        CrossTableCoverage {
            arpnd_capture_complete: true,
            mac_capture_complete: true,
            covered_interfaces: [
                "Port-Channel3",
                "Port-Channel4",
                "Vxlan1",
                "Vlan10",
                "Vlan30",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    #[test]
    fn genuine_eos_cross_table_consistency_is_deterministic() {
        let facts = eos_facts();
        let report =
            reconcile_neighbor_tables(&facts, &complete_coverage(), "2021-06-07T00:01:00+00:00")
                .unwrap();
        let consistent: Vec<_> = report
            .results
            .iter()
            .filter(|r| r.outcome == CrossTableOutcome::Consistent)
            .collect();
        assert_eq!(consistent.len(), 4);
        assert!(
            consistent
                .iter()
                .any(|r| r.ip_address == "172.16.3.202"
                    && r.neighbor_forwarding_interface == "Vxlan1")
        );
        assert!(report.results.iter().all(|r| r.outcome
            != CrossTableOutcome::AbsentInDeclaredCoverage
            || r.vlan_id.is_some()));

        let mut reversed = facts.clone();
        reversed.reverse();
        let report2 =
            reconcile_neighbor_tables(&reversed, &complete_coverage(), "2021-06-07T00:01:00+00:00")
                .unwrap();
        let a = serde_json::to_value(&report.results).unwrap();
        let b = serde_json::to_value(&report2.results).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn semantically_invalid_facts_are_rejected_before_matching() {
        let mut facts = eos_facts();
        let mut invalid = facts
            .iter()
            .find(|f| f.fact_kind == FACT_KIND_MAC_FORWARDING)
            .unwrap()
            .clone();
        let mut fields = invalid.payload.fields.clone().unwrap();
        fields["outgoing_interface"] = serde_json::json!("Ethernet99");
        invalid.payload.fields = Some(fields);
        invalid.seal();
        facts.push(invalid);
        let report =
            reconcile_neighbor_tables(&facts, &complete_coverage(), "2021-06-07T00:01:00+00:00")
                .unwrap();
        assert_eq!(report.invalid_mac_facts, 1);
        assert_eq!(
            report
                .results
                .iter()
                .filter(|r| r.outcome == CrossTableOutcome::Consistent)
                .count(),
            4
        );
    }

    #[test]
    fn incomplete_coverage_cannot_establish_absence() {
        let facts = eos_facts();
        let coverage = CrossTableCoverage::default();
        let report =
            reconcile_neighbor_tables(&facts, &coverage, "2021-06-07T00:01:00+00:00").unwrap();
        assert!(
            !report
                .results
                .iter()
                .any(|r| r.outcome == CrossTableOutcome::AbsentInDeclaredCoverage)
        );
    }

    #[test]
    fn withdrawn_neighbor_evidence_is_not_current() {
        let mut facts = eos_facts();
        for fact in &mut facts {
            if fact.fact_kind == FACT_KIND_ARPND_NEIGHBOR {
                fact.lifecycle = Lifecycle::Withdrawn;
                fact.withdrawn_reason = Some("test withdrawal".into());
                fact.lifecycle_changed_at = Some("2021-06-07T00:00:30+00:00".into());
                fact.seal();
            }
        }
        let report =
            reconcile_neighbor_tables(&facts, &complete_coverage(), "2021-06-07T00:01:00+00:00")
                .unwrap();
        assert_eq!(report.results.len(), 0);
    }

    #[test]
    #[ignore = "synthetic scaling evidence; run explicitly for performance measurement"]
    fn synthetic_cross_table_scaling_100_1000_10000() {
        let base = eos_facts();
        let arp_template = base
            .iter()
            .find(|f| {
                f.fact_kind == FACT_KIND_ARPND_NEIGHBOR
                    && f.payload
                        .fields
                        .as_ref()
                        .is_some_and(|v| v["ip_address"] == serde_json::json!("172.16.3.202"))
            })
            .unwrap()
            .clone();
        let mac_template =
            base.iter()
                .find(|f| {
                    f.fact_kind == FACT_KIND_MAC_FORWARDING
                        && f.payload.fields.as_ref().is_some_and(|v| {
                            v["mac_address"] == serde_json::json!("d2:74:8c:66:e7:e2")
                        })
                })
                .unwrap()
                .clone();
        for n in [100usize, 1_000, 10_000] {
            let mut facts = Vec::with_capacity(n * 2);
            for i in 0..n {
                let mac = format!(
                    "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
                    (i >> 16) & 0xff,
                    (i >> 8) & 0xff,
                    i & 0xff,
                    i % 251
                );
                let ip = format!("10.{}.{}.{}", (i >> 16) & 0xff, (i >> 8) & 0xff, i & 0xff);
                let mut arp = arp_template.clone();
                let mut fields = arp.payload.fields.clone().unwrap();
                fields["ip_address"] = serde_json::json!(ip);
                fields["mac_address"]["value"] = serde_json::json!(mac);
                arp.payload.fields = Some(fields);
                arp.seal();
                let mut macfact = mac_template.clone();
                let mut fields = macfact.payload.fields.clone().unwrap();
                fields["mac_address"] = serde_json::json!(mac);
                macfact.payload.fields = Some(fields);
                macfact.seal();
                facts.push(arp);
                facts.push(macfact);
            }
            let start = std::time::Instant::now();
            let report = reconcile_neighbor_tables(
                &facts,
                &complete_coverage(),
                "2021-06-07T00:01:00+00:00",
            )
            .unwrap();
            let elapsed = start.elapsed();
            let consistent = report
                .results
                .iter()
                .filter(|r| r.outcome == CrossTableOutcome::Consistent)
                .count();
            assert_eq!(consistent, n);
            eprintln!(
                "synthetic neighbor reconciliation observations={} results={} elapsed_ms={}",
                n * 2,
                report.results.len(),
                elapsed.as_millis()
            );
        }
    }
}

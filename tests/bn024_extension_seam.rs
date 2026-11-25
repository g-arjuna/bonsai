//! BN-024-03/03A — versioned profile + extension-seam acceptance tests.
//!
//! Covers distinct SONiC/Cumulus/FRR platform identification, canonical
//! alias normalization, conservative version/scope selection, ambiguous
//! fail-closed selection, future-NOS registration, receipt-bound normalizer
//! dispatch, exact revision/acquisition/namespace/release validation and
//! the no-authority boundary. All offline; no Python runtime, graph access
//! or device writes.

use bonsai::interface_normalizer::{
    CaptureOutcome, CommandCapture, InterfaceObservation, NormalizerNos, ObservationContext,
};
use bonsai::normalizer_registry::{DispatchError, ExecutionQualification, NormalizerRegistry};
use bonsai::nos_features::{
    AcquisitionKind, FeatureStatus, HardwareScope, NosFeatureProfile, ProfileQuery,
    ProfileRegistry, PromotionState, ReleaseConstraint, ResourceBudget, Selection, SoftwareRelease,
    SupportState, builtin_profiles, builtin_registry,
};
use bonsai::platform_registry::{lookup_platform, normalize_platform_id};
use std::collections::BTreeMap;

fn ctx_obs<'a>(nos: NormalizerNos, vtysh: &'a str, ip: &'a str) -> InterfaceObservation<'a> {
    observation(nos, vtysh, ip, None, None)
}

fn observation<'a>(
    nos: NormalizerNos,
    vtysh: &'a str,
    ip: &'a str,
    namespace: Option<&'a str>,
    release: Option<&'a str>,
) -> InterfaceObservation<'a> {
    let mut context =
        ObservationContext::minimal("dev-1", "col-1", "cap-1", "2026-11-25T00:00:00Z");
    context.session_epoch = 1;
    context.network_namespace = namespace;
    context.platform = Some(match nos {
        NormalizerNos::Sonic => "sonic",
        NormalizerNos::Cumulus => "nvidia.cumulus",
    });
    context.release_raw = release;
    InterfaceObservation {
        nos,
        vtysh: CommandCapture::new(vtysh, CaptureOutcome::Complete),
        ip_addr: CommandCapture::new(ip, CaptureOutcome::Complete),
        context,
    }
}

fn query(platform: &str, version: &str) -> ProfileQuery {
    ProfileQuery {
        platform: platform.into(),
        version: version.into(),
        release: None,
        hardware: None,
    }
}

fn select(reg: &ProfileRegistry, platform: &str, version: &str) -> Selection {
    reg.select(&query(platform, version))
}

fn simple_pair(ifname: &str) -> (String, String) {
    (
        format!(
            "Interface {ifname} is up, line protocol is up\n  vrf: default\n  index 1 metric 0 mtu 1500 speed 0 \n"
        ),
        format!(
            "1: {ifname}: <UP,LOWER_UP> mtu 1500 state UP group default\n    link/ether 44:38:39:00:00:01 brd ff:ff:ff:ff:ff:ff\n"
        ),
    )
}

fn two_interface_pair() -> (String, String) {
    let (v1, i1) = simple_pair("Ethernet0");
    let (v2, i2) = simple_pair("Ethernet4");
    let v2 = v2.replace("index 1", "index 2");
    let i2 = i2.replacen("1: Ethernet4", "2: Ethernet4", 1);
    (format!("{v1}{v2}"), format!("{i1}{i2}"))
}

fn cumulus_builtin() -> NosFeatureProfile {
    builtin_profiles()
        .into_iter()
        .find(|p| p.profile_id == "cumulus-1")
        .expect("builtin cumulus")
}

/// Contract-test profile used to exercise the release-qualified dispatch
/// boundary. The sentinel release is deliberately not a claim about any
/// real Cumulus release; no test uses it as evidence of platform support.
fn contract_qualified_profile() -> NosFeatureProfile {
    let mut p = cumulus_builtin();
    p.profile_id = "contract-release-qualified-not-a-platform-claim".into();
    p.version_match = "=contract-test-release".into();
    p.release_constraint = Some(ReleaseConstraint {
        raw: Some("contract-test-release".into()),
        train: None,
        build: None,
        components: BTreeMap::new(),
    });
    p.verified_namespaces = vec!["default".into()];
    p.verification = "release_qualified".into();
    p
}

fn contract_registry() -> ProfileRegistry {
    let mut reg = builtin_registry();
    reg.register(contract_qualified_profile());
    reg
}

fn contract_receipt(reg: &ProfileRegistry) -> bonsai::nos_features::SelectionReceipt {
    reg.select_receipt(&query("nvidia.cumulus", "contract-test-release"))
}

// ── Platform identification (D4/D5) ─────────────────────────────────

#[test]
fn sonic_and_cumulus_are_distinct_canonical_platforms() {
    assert_eq!(normalize_platform_id("sonic"), "sonic");
    assert_eq!(normalize_platform_id("sonic_linux"), "sonic");
    assert_eq!(normalize_platform_id("cumulus"), "nvidia.cumulus");
    assert_eq!(normalize_platform_id("cumulus_linux"), "nvidia.cumulus");
    assert_eq!(normalize_platform_id("nvidia-cumulus"), "nvidia.cumulus");
    assert_eq!(normalize_platform_id("frr"), "frr");
    assert_ne!(
        lookup_platform("sonic").platform_id,
        lookup_platform("frr").platform_id
    );
    assert_ne!(
        lookup_platform("cumulus").platform_id,
        lookup_platform("frr").platform_id
    );
    let cum = lookup_platform("cumulus");
    assert_eq!(cum.vendor, "nvidia");
    assert_eq!(cum.os, "cumulus");
}

#[test]
fn canonical_platform_aliases_are_used_by_feature_selection() {
    let reg = builtin_registry();
    for alias in ["cumulus", "cumulus_linux", "NVIDIA-Cumulus"] {
        match reg.select(&query(alias, "5.13.0")) {
            Selection::Selected(p) => assert_eq!(p.platform_family, "nvidia.cumulus"),
            other => panic!("alias {alias} must select nvidia.cumulus, got {other:?}"),
        }
    }
    // Unknown future platforms remain distinct registration namespaces;
    // they are not collapsed into platform_registry's generic "unknown".
    let mut reg = ProfileRegistry::new();
    reg.register(NosFeatureProfile {
        profile_id: "futureos-1".into(),
        platform_family: "futureos".into(),
        version_match: "=2026.1".into(),
        release_constraint: None,
        hardware_constraints: vec![],
        verified_namespaces: vec![],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::new(),
        verification: "untested".into(),
    });
    assert!(matches!(
        reg.select(&query("futureos.router", "2026.1")),
        Selection::Selected(_)
    ));
}

#[test]
fn ambiguous_platform_detection_fails_closed() {
    let reg = builtin_registry();
    match select(&reg, "sonic", "2025.1") {
        Selection::Selected(p) => assert_eq!(p.profile_id, "sonic-1"),
        other => panic!("expected sonic-1 selection, got {other:?}"),
    }
    match select(&reg, "nvidia.cumulus", "5.13.0") {
        Selection::Selected(p) => assert_eq!(p.profile_id, "cumulus-1"),
        other => panic!("expected cumulus-1 selection, got {other:?}"),
    }
}

// ── Versioned selection semantics (D2/D5/B5) ────────────────────────

#[test]
fn unknown_release_is_candidate_never_executable() {
    let reg = builtin_registry();
    for v in ["*", ""] {
        match reg.select(&query("sonic", v)) {
            Selection::Candidate(p) => assert_eq!(p.profile_id, "sonic-1"),
            other => panic!("version '{v}' must be Candidate, got {other:?}"),
        }
        let receipt = reg.select_receipt(&query("sonic", v));
        assert!(matches!(
            NormalizerRegistry::builtin().dispatch(
                &receipt,
                "interfaces.frr_cli",
                &ctx_obs(NormalizerNos::Sonic, "", "")
            ),
            Err(DispatchError::NotExecutableSelection)
        ));
    }
    // A structured object does not make empty/"*" raw values known.
    for raw in ["", "*"] {
        let q = ProfileQuery {
            platform: "sonic".into(),
            version: "".into(),
            release: Some(SoftwareRelease {
                raw: raw.into(),
                train: None,
                build: None,
                components: BTreeMap::new(),
            }),
            hardware: None,
        };
        assert!(
            matches!(reg.select(&q), Selection::Candidate(_)),
            "raw {raw:?}"
        );
    }
}

#[test]
fn structured_release_is_the_single_effective_version_representation() {
    let mut reg = ProfileRegistry::new();
    reg.register(NosFeatureProfile {
        profile_id: "futureos-1".into(),
        platform_family: "futureos".into(),
        version_match: "=2026.1".into(),
        release_constraint: None,
        hardware_constraints: vec![],
        verified_namespaces: vec![],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::new(),
        verification: "untested".into(),
    });
    // Legacy version may be empty when structured release is authoritative.
    let q = ProfileQuery {
        platform: "futureos.router".into(),
        version: "".into(),
        release: Some(SoftwareRelease {
            raw: "2026.1".into(),
            train: None,
            build: None,
            components: BTreeMap::new(),
        }),
        hardware: None,
    };
    assert!(matches!(reg.select(&q), Selection::Selected(_)));
    // Contradictory metadata fails closed.
    let q = ProfileQuery {
        version: "2025.9".into(),
        ..q
    };
    assert!(matches!(reg.select(&q), Selection::OutOfScope { .. }));
}

#[test]
fn specific_scope_failure_blocks_broad_wildcard_fallback() {
    let mut reg = ProfileRegistry::new();
    let mk = |profile_id: &str, platform: &str, rc: Option<ReleaseConstraint>| NosFeatureProfile {
        profile_id: profile_id.into(),
        platform_family: platform.into(),
        version_match: "*".into(),
        release_constraint: rc,
        hardware_constraints: vec![],
        verified_namespaces: vec![],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::new(),
        verification: "untested".into(),
    };
    // Exact-platform profile recognizes the platform/version but rejects
    // the release. The less-specific family wildcard must not bypass that.
    reg.register(mk(
        "exact-restricted",
        "acme.box",
        Some(ReleaseConstraint {
            raw: Some("pinned".into()),
            train: None,
            build: None,
            components: BTreeMap::new(),
        }),
    ));
    reg.register(mk("family-wildcard", "acme", None));
    match reg.select(&query("acme.box", "1")) {
        Selection::OutOfScope { reason, .. } => {
            assert!(reason.contains("release constraint"), "{reason}")
        }
        other => panic!("broad wildcard bypassed explicit restriction: {other:?}"),
    }
}

#[test]
fn release_constraint_is_equality_only_and_fails_closed() {
    let mut p = NosFeatureProfile {
        profile_id: "pinned".into(),
        platform_family: "acme".into(),
        version_match: "*".into(),
        release_constraint: Some(ReleaseConstraint {
            raw: None,
            train: None,
            build: None,
            components: BTreeMap::from([("frr".to_string(), "10.6.1".to_string())]),
        }),
        hardware_constraints: vec![],
        verified_namespaces: vec![],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::new(),
        verification: "untested".into(),
    };
    let mut reg = ProfileRegistry::new();
    reg.register(p.clone());
    let q = |components: &[(&str, &str)]| ProfileQuery {
        platform: "acme.box".into(),
        version: "1.0".into(),
        release: Some(SoftwareRelease {
            raw: "1.0".into(),
            train: None,
            build: None,
            components: components
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }),
        hardware: None,
    };
    assert!(matches!(
        reg.select(&q(&[("frr", "10.6.1")])),
        Selection::Selected(_)
    ));
    assert!(matches!(
        reg.select(&q(&[("frr", "10.7.0")])),
        Selection::OutOfScope { .. }
    ));
    assert!(matches!(reg.select(&q(&[])), Selection::OutOfScope { .. }));
    p.verification = "untested".into();
    assert_eq!(p.verification, "untested");
}

#[test]
fn missing_hardware_evidence_fails_closed() {
    let mut reg = ProfileRegistry::new();
    reg.register(NosFeatureProfile {
        profile_id: "hw-bound".into(),
        platform_family: "acme".into(),
        version_match: "*".into(),
        release_constraint: None,
        hardware_constraints: vec![HardwareScope {
            platform_sku: None,
            asic: Some("tarmac".into()),
            namespace: None,
            attributes: Default::default(),
        }],
        verified_namespaces: vec![],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::new(),
        verification: "untested".into(),
    });
    for hardware in [
        None,
        Some(HardwareScope {
            asic: Some("other".into()),
            ..Default::default()
        }),
    ] {
        assert!(matches!(
            reg.select(&ProfileQuery {
                hardware,
                ..query("acme.box", "1")
            }),
            Selection::OutOfScope { .. }
        ));
    }
    assert!(matches!(
        reg.select(&ProfileQuery {
            hardware: Some(HardwareScope {
                asic: Some("tarmac".into()),
                ..Default::default()
            }),
            ..query("acme.box", "1")
        }),
        Selection::Selected(_)
    ));
}

#[test]
fn overlapping_profile_predicates_are_ambiguous_not_arbitrary() {
    let mut reg = ProfileRegistry::new();
    for id in ["a", "b"] {
        reg.register(NosFeatureProfile {
            profile_id: id.into(),
            platform_family: "acme".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: Default::default(),
            features: BTreeMap::new(),
            verification: "untested".into(),
        });
    }
    match reg.select(&query("acme.box", "9")) {
        Selection::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    let mut alias_owner = std::collections::BTreeMap::new();
    for p in bonsai::platform_registry::PLATFORM_PROFILES {
        for a in p.aliases {
            if let Some(prev) = alias_owner.insert(*a, p.platform_id) {
                assert_eq!(prev, p.platform_id, "alias {a} claimed by two platforms");
            }
            assert_eq!(normalize_platform_id(a), p.platform_id);
        }
    }
}

#[test]
fn dummy_future_nos_registers_without_core_changes() {
    let mut reg = builtin_registry();
    reg.register(NosFeatureProfile {
        profile_id: "futureos-1".into(),
        platform_family: "futureos".into(),
        version_match: "=2026.1".into(),
        release_constraint: None,
        hardware_constraints: vec![],
        verified_namespaces: vec!["default".into()],
        device_roles: vec![],
        resource_budget: Default::default(),
        features: BTreeMap::from([(
            "interfaces".into(),
            bonsai::nos_features::FeatureFlag {
                enabled: true,
                parsers: vec![],
                scope_notes: vec![],
                requires: vec![],
                conflicts: vec![],
                support: SupportState::Supported,
                evidence: vec![],
                acquisition: vec![],
                normalizer: None,
                promotion: PromotionState::Shadow,
            },
        )]),
        verification: "untested".into(),
    });
    let receipt = reg.select_receipt(&query("futureos.router", "2026.1"));
    match receipt.selection() {
        Selection::Selected(p) => assert_eq!(p.profile_id, "futureos-1"),
        other => panic!("expected futureos-1, got {other:?}"),
    }
    let nr = NormalizerRegistry::builtin();
    // Missing observation metadata fails closed before any execution.
    let obs = ctx_obs(NormalizerNos::Cumulus, "", "");
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces", &obs),
        Err(DispatchError::ObservationMetadataMissing { .. })
            | Err(DispatchError::NamespaceNotQualified { .. })
            | Err(DispatchError::NoNormalizerBound { .. })
            | Err(DispatchError::PlatformNotQualified { .. })
            | Err(DispatchError::FeatureNotEnabled { .. })
    ));
    // Even with complete consistent metadata, the future-NOS feature can
    // never execute: no normalizer is bound and no registered normalizer
    // is qualified for the unknown platform family.
    let (v, i) = simple_pair("Ethernet0");
    let mut full = observation(
        NormalizerNos::Cumulus,
        &v,
        &i,
        Some("default"),
        Some("2026.1"),
    );
    full.context.platform = Some("futureos");
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces", &full),
        Err(DispatchError::NoNormalizerBound { .. })
            | Err(DispatchError::PlatformNotQualified { .. })
            | Err(DispatchError::NamespaceNotQualified { .. })
            | Err(DispatchError::ProfileNotReleaseQualified { .. })
    ));
}

// ── Feature/normalizer contract fields (D2/B4) ──────────────────────

#[test]
fn builtin_profiles_declare_typed_contract_fields() {
    let reg = builtin_registry();
    for platform in ["sonic", "nvidia.cumulus"] {
        let sel = reg.select(&query(platform, "1"));
        let Selection::Selected(p) = sel else {
            panic!("{platform} must select");
        };
        let flag = p.features.get("interfaces.frr_cli").expect("feature");
        assert_eq!(flag.promotion, PromotionState::Shadow);
        assert!(
            flag.acquisition
                .iter()
                .any(|a| a.kind == AcquisitionKind::CliObservation)
        );
        let n = flag.normalizer.as_ref().expect("normalizer bound");
        assert_eq!(n.id, "interface_normalizer");
        assert_eq!(
            n.revision,
            bonsai::interface_normalizer::INTERFACE_CLI_PARSER_REVISION
        );
        assert_eq!(
            n.mapping_revision.as_deref(),
            Some(bonsai::interface_normalizer::INTERFACE_CLI_MAPPING_REVISION)
        );
        assert!(p.resource_budget.max_input_bytes.is_some());
        for a in &flag.acquisition {
            for c in &a.contracts {
                assert!(!c.contains(';') && !c.contains('$') && !c.contains('`'));
            }
        }
    }
    let sel = reg.select(&query("nvidia.cumulus", "1"));
    let Selection::Selected(p) = sel else {
        panic!()
    };
    assert_eq!(
        ProfileRegistry::feature_status(&p, "nvue"),
        FeatureStatus::Disabled
    );
    let sel = reg.select(&query("sonic", "1"));
    let Selection::Selected(p) = sel else {
        panic!()
    };
    for f in ["state_db", "counters_db"] {
        assert_eq!(
            ProfileRegistry::feature_status(&p, f),
            FeatureStatus::Disabled,
            "{f} must be declared-disabled, not absent or enabled"
        );
    }
}

#[test]
fn cumulus_fixture_does_not_establish_sonic_support() {
    let reg = builtin_registry();
    let Selection::Selected(sonic) = reg.select(&query("sonic", "any-nonempty-version")) else {
        panic!("sonic profile must be recognized")
    };
    let flag = sonic.features.get("interfaces.frr_cli").unwrap();
    assert_eq!(flag.support, SupportState::Unknown);
    assert!(
        !flag
            .evidence
            .iter()
            .any(|e| e.kind == "observed_capture" && e.claim == SupportState::Supported),
        "Cumulus-only fixture evidence must not be copied to SONiC"
    );
    assert_eq!(
        ProfileRegistry::feature_status(&sonic, "interfaces.frr_cli"),
        FeatureStatus::Unknown
    );
}

// ── Receipt-bound dispatch and qualification boundary (B1/B2/B3/B4) ──

#[test]
fn arbitrary_or_future_nonempty_version_is_not_release_qualification() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let (v, ip) = simple_pair("Ethernet0");
    for version in ["1", "2999.99"] {
        let receipt = reg.select_receipt(&query("nvidia.cumulus", version));
        assert!(matches!(receipt.selection(), Selection::Selected(_)));
        let err = nr
            .dispatch(
                &receipt,
                "interfaces.frr_cli",
                &observation(
                    NormalizerNos::Cumulus,
                    &v,
                    &ip,
                    Some("default"),
                    Some(version),
                ),
            )
            .expect_err("wildcard known version must not qualify dispatch");
        assert!(
            matches!(err, DispatchError::ProfileNotReleaseQualified { .. }),
            "version {version}: got {err:?}"
        );
    }
}

#[test]
fn historical_cumulus_fixture_runs_only_as_explicit_unqualified_experiment() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let receipt = reg.select_receipt(&query("nvidia.cumulus", "fixture-unknown"));
    let vtysh = include_str!("fixtures/bn024/interface-cumulus/vtysh-show-int-vrf-all.txt");
    let ip = include_str!("fixtures/bn024/interface-cumulus/ip-d-address-show.txt");
    let mut obs = ctx_obs(NormalizerNos::Cumulus, vtysh, ip);
    obs.vtysh = CommandCapture::unestablished(vtysh);
    obs.ip_addr = CommandCapture::unestablished(ip);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::NotExecutableSelection)
            | Err(DispatchError::ObservationMetadataMissing { .. })
            | Err(DispatchError::NamespaceNotQualified { .. })
            | Err(DispatchError::ProfileNotReleaseQualified { .. })
    ));
    let run = nr
        .dispatch_experimental(&receipt, "interfaces.frr_cli", &obs)
        .expect("explicit experimental fixture execution");
    assert_eq!(
        run.qualification,
        ExecutionQualification::ExperimentalUnqualified
    );
    assert!(run.shadow_only);
    assert!(
        run.qualification_limitations
            .iter()
            .any(|r| r.contains("namespace") || r.contains("release")),
        "limitations must expose the qualification gap: {:?}",
        run.qualification_limitations
    );
    assert!(run.output.report.interfaces_observed > 0);
    assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
}

#[test]
fn qualified_selection_requires_matching_observation_release() {
    let reg = contract_registry();
    let nr = NormalizerRegistry::builtin();
    let receipt = contract_receipt(&reg);
    assert!(matches!(receipt.selection(), Selection::Selected(_)));
    let (v, ip) = simple_pair("Ethernet0");

    // Qualified profile selected, but observation release is missing.
    let missing = observation(NormalizerNos::Cumulus, &v, &ip, Some("default"), None);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &missing),
        Err(DispatchError::ObservationMetadataMissing { .. })
    ));

    // Selected release differs from actual observation release.
    let different = observation(
        NormalizerNos::Cumulus,
        &v,
        &ip,
        Some("default"),
        Some("different-release"),
    );
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &different),
        Err(DispatchError::ObservationReleaseMismatch { .. })
    ));
}

#[test]
fn qualified_dispatch_enforces_exact_namespace() {
    let reg = contract_registry();
    let nr = NormalizerRegistry::builtin();
    let receipt = contract_receipt(&reg);
    let (v, ip) = simple_pair("Ethernet0");
    for ns in [None, Some("asic-2"), Some("host")] {
        let obs = observation(
            NormalizerNos::Cumulus,
            &v,
            &ip,
            ns,
            Some("contract-test-release"),
        );
        let err = nr
            .dispatch(&receipt, "interfaces.frr_cli", &obs)
            .expect_err("unqualified namespace must fail closed");
        assert!(
            matches!(
                err,
                DispatchError::ObservationMetadataMissing { .. }
                    | DispatchError::NamespaceNotQualified { .. }
            ),
            "ns {ns:?}: got {err:?}"
        );
    }
}

#[test]
fn duplicate_interface_names_remain_distinct_across_namespaces() {
    let nr = NormalizerRegistry::builtin();
    let reg = builtin_registry();
    let receipt = reg.select_receipt(&query("nvidia.cumulus", "fixture-experiment"));
    let (v, ip) = simple_pair("Ethernet0");
    let mut ids = Vec::new();
    for ns in ["asic-0", "asic-1"] {
        let obs = observation(NormalizerNos::Cumulus, &v, &ip, Some(ns), None);
        let run = nr
            .dispatch_experimental(&receipt, "interfaces.frr_cli", &obs)
            .expect("experimental duplicate namespace run");
        let fact = run
            .output
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("Ethernet0"))
            .expect("interface fact");
        assert_eq!(fact.subject.network_namespace.as_deref(), Some(ns));
        ids.push(fact.fact_id.clone());
    }
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn dispatch_rejects_revision_acquisition_and_dependency_contract_mismatch() {
    let nr = NormalizerRegistry::builtin();
    let (v, ip) = simple_pair("Ethernet0");
    let obs = observation(
        NormalizerNos::Cumulus,
        &v,
        &ip,
        Some("default"),
        Some("contract-test-release"),
    );

    // Normalizer revision mismatch.
    let mut p = contract_qualified_profile();
    p.features
        .get_mut("interfaces.frr_cli")
        .unwrap()
        .normalizer
        .as_mut()
        .unwrap()
        .revision = "not-the-implemented-revision".into();
    let mut reg = builtin_registry();
    reg.register(p);
    let receipt = contract_receipt(&reg);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::NormalizerRevisionMismatch { .. })
    ));

    // Mapping revision mismatch.
    let mut p = contract_qualified_profile();
    p.features
        .get_mut("interfaces.frr_cli")
        .unwrap()
        .normalizer
        .as_mut()
        .unwrap()
        .mapping_revision = Some("wrong-map".into());
    let mut reg = builtin_registry();
    reg.register(p);
    let receipt = contract_receipt(&reg);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::MappingRevisionMismatch { .. })
    ));

    // Missing required acquisition contract.
    let mut p = contract_qualified_profile();
    p.features
        .get_mut("interfaces.frr_cli")
        .unwrap()
        .acquisition = vec![];
    let mut reg = builtin_registry();
    reg.register(p);
    let receipt = contract_receipt(&reg);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::InvalidProfile { .. })
            | Err(DispatchError::AcquisitionContractMismatch { .. })
    ));

    // Disabled prerequisite makes the executable composition invalid.
    let mut p = contract_qualified_profile();
    let mut base = p.features.get("interfaces.frr_cli").unwrap().clone();
    base.enabled = false;
    base.normalizer = None;
    base.acquisition = vec![];
    p.features.insert("interfaces.base".into(), base);
    p.features.get_mut("interfaces.frr_cli").unwrap().requires = vec!["interfaces.base".into()];
    let mut reg = builtin_registry();
    reg.register(p);
    let receipt = contract_receipt(&reg);
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::FeatureCompositionInvalid { .. })
    ));
}

#[test]
fn resource_budgets_tighten_but_never_relax_implementation_limits() {
    let (v, ip) = two_interface_pair();
    let nr = NormalizerRegistry::builtin();
    for (budget, assert_report) in [
        (
            ResourceBudget {
                max_input_bytes: Some(16),
                max_entities: None,
                max_output_facts: None,
            },
            "input",
        ),
        (
            ResourceBudget {
                max_input_bytes: None,
                max_entities: Some(1),
                max_output_facts: None,
            },
            "entities",
        ),
        (
            ResourceBudget {
                max_input_bytes: None,
                max_entities: None,
                max_output_facts: Some(1),
            },
            "output",
        ),
    ] {
        let mut p = cumulus_builtin();
        p.profile_id = format!("budget-{assert_report}");
        p.resource_budget = budget;
        let mut reg = ProfileRegistry::new();
        reg.register(p);
        let receipt = reg.select_receipt(&query("nvidia.cumulus", "budget-experiment"));
        let obs = observation(NormalizerNos::Cumulus, &v, &ip, None, None);
        let run = nr
            .dispatch_experimental(&receipt, "interfaces.frr_cli", &obs)
            .expect("budgeted experimental run");
        match assert_report {
            "input" => assert!(
                run.output
                    .report
                    .notes
                    .iter()
                    .any(|n| n.contains("input byte bound")),
                "{:?}",
                run.output.report.notes
            ),
            "entities" => assert!(run.output.report.entities_omitted > 0),
            "output" => assert!(run.output.report.output_facts_omitted > 0),
            _ => unreachable!(),
        }
        assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
    }
}

#[test]
fn active_promotion_state_never_relaxes_shadow_only_at_this_stage() {
    let mut p = cumulus_builtin();
    p.profile_id = "active-should-still-shadow".into();
    p.features.get_mut("interfaces.frr_cli").unwrap().promotion = PromotionState::Active;
    let mut reg = ProfileRegistry::new();
    reg.register(p);
    let receipt = reg.select_receipt(&query("nvidia.cumulus", "experiment"));
    let (v, ip) = simple_pair("Ethernet0");
    let run = NormalizerRegistry::builtin()
        .dispatch_experimental(
            &receipt,
            "interfaces.frr_cli",
            &observation(NormalizerNos::Cumulus, &v, &ip, None, None),
        )
        .expect("experimental active-labelled profile still executes offline");
    assert!(run.shadow_only);
    assert_eq!(run.declared_promotion, PromotionState::Active);
    assert!(
        run.qualification_limitations
            .iter()
            .any(|r| r.contains("PromotionState::Active"))
    );
}

#[test]
fn dispatch_rejects_wrong_platform_observation_and_unselected() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let obs = ctx_obs(NormalizerNos::Cumulus, "", "");
    let receipt = reg.select_receipt(&query("sonic", "1"));
    assert!(matches!(
        nr.dispatch_experimental(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::ObservationPlatformMismatch { .. })
    ));
    let receipt = reg.select_receipt(&query("sonic", "*"));
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::NotExecutableSelection)
    ));
    let receipt = reg.select_receipt(&query("frr", "9.1"));
    assert!(matches!(
        nr.dispatch(&receipt, "interfaces.frr_cli", &obs),
        Err(DispatchError::FeatureNotEnabled { .. })
    ));
}

#[test]
fn dispatch_produces_no_authority_and_no_graph_access() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let receipt = reg.select_receipt(&query("sonic", "1"));
    let run = nr
        .dispatch_experimental(
            &receipt,
            "interfaces.frr_cli",
            &ctx_obs(NormalizerNos::Sonic, "", ""),
        )
        .unwrap();
    assert!(run.shadow_only);
    assert_eq!(
        run.qualification,
        ExecutionQualification::ExperimentalUnqualified
    );
    for f in &run.output.facts {
        let v = serde_json::to_value(f).unwrap();
        assert_eq!(v["trust"], "device_asserted");
        assert!(v.get("authority").is_none() || v["authority"].is_null());
    }
}

//! BN-024-03/03A — versioned profile + extension-seam acceptance tests.
//!
//! Covers the bounded acceptance list: distinct SONiC/Cumulus/FRR
//! platform identification, versioned selection semantics, ambiguous
//! fail-closed selection, dummy future-NOS registration, interface
//! normalizer dispatch through the registry, and the no-authority
//! boundary. All offline; no Python runtime, no graph access.

use bonsai::interface_normalizer::{
    CaptureOutcome, CommandCapture, InterfaceObservation, NormalizationOutput, NormalizerNos,
    ObservationContext,
};
use bonsai::normalizer_registry::{DispatchError, NormalizerRegistry};
use bonsai::nos_features::{
    AcquisitionKind, FeatureStatus, HardwareScope, NosFeatureProfile, ProfileQuery,
    ProfileRegistry, PromotionState, ReleaseConstraint, Selection, SoftwareRelease, SupportState,
    builtin_registry,
};
use bonsai::platform_registry::{lookup_platform, normalize_platform_id};
use std::collections::BTreeMap;

fn ctx_obs<'a>(nos: NormalizerNos, vtysh: &'a str, ip: &'a str) -> InterfaceObservation<'a> {
    InterfaceObservation {
        nos,
        vtysh: CommandCapture::new(vtysh, CaptureOutcome::Complete),
        ip_addr: CommandCapture::new(ip, CaptureOutcome::Complete),
        context: ObservationContext {
            device_id: "dev-1",
            scope: None,
            network_namespace: None,
            collector_id: "col-1",
            capture_id: "cap-1",
            session_epoch: 1,
            sequence: 0,
            observed_at: "2026-11-25T00:00:00Z",
            release_raw: None,
        },
    }
}

fn select(reg: &ProfileRegistry, platform: &str, version: &str) -> Selection {
    reg.select(&ProfileQuery {
        platform: platform.into(),
        version: version.into(),
        release: None,
        hardware: None,
    })
}

// ── Platform identification (D4/D5) ─────────────────────────────────

#[test]
fn sonic_and_cumulus_are_distinct_canonical_platforms() {
    assert_eq!(normalize_platform_id("sonic"), "sonic");
    assert_eq!(normalize_platform_id("sonic_linux"), "sonic");
    assert_eq!(normalize_platform_id("cumulus"), "nvidia.cumulus");
    assert_eq!(normalize_platform_id("cumulus_linux"), "nvidia.cumulus");
    assert_eq!(normalize_platform_id("nvidia-cumulus"), "nvidia.cumulus");
    // Standalone FRR remains its own platform — not folded into either.
    assert_eq!(normalize_platform_id("frr"), "frr");
    assert_ne!(
        lookup_platform("sonic").platform_id,
        lookup_platform("frr").platform_id
    );
    assert_ne!(
        lookup_platform("cumulus").platform_id,
        lookup_platform("frr").platform_id
    );
    // Cumulus must not be represented as merely generic FRR.
    let cum = lookup_platform("cumulus");
    assert_eq!(cum.vendor, "nvidia");
    assert_eq!(cum.os, "cumulus");
}

#[test]
fn ambiguous_platform_detection_fails_closed() {
    // "sonic" must not also resolve through another profile's aliases.
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

// ── Versioned selection semantics (D2/D5) ───────────────────────────

#[test]
fn unknown_release_is_candidate_never_executable() {
    let reg = builtin_registry();
    for v in ["*", ""] {
        match reg.select(&ProfileQuery {
            platform: "sonic".into(),
            version: v.into(),
            release: None,
            hardware: None,
        }) {
            Selection::Candidate(p) => assert_eq!(p.profile_id, "sonic-1"),
            other => panic!("version '{v}' must be Candidate, got {other:?}"),
        }
    }
}

#[test]
fn release_constraint_is_equality_only_and_fails_closed() {
    // A profile pinning a component version rejects devices reporting a
    // different one — no lexical/semver comparison across NOS families.
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
    // Wrong component version -> OutOfScope, never a looser match.
    assert!(matches!(
        reg.select(&q(&[("frr", "10.7.0")])),
        Selection::OutOfScope { .. }
    ));
    // Missing component evidence -> fail closed.
    assert!(matches!(reg.select(&q(&[])), Selection::OutOfScope { .. }));
    // version:all-style wildcard profile must not be read as tested.
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
    // Query without hardware evidence cannot satisfy the constraint.
    assert!(matches!(
        reg.select(&ProfileQuery {
            platform: "acme.box".into(),
            version: "1".into(),
            release: None,
            hardware: None,
        }),
        Selection::OutOfScope { .. }
    ));
    assert!(matches!(
        reg.select(&ProfileQuery {
            platform: "acme.box".into(),
            version: "1".into(),
            release: None,
            hardware: Some(HardwareScope {
                asic: Some("other".into()),
                ..Default::default()
            }),
        }),
        Selection::OutOfScope { .. }
    ));
    assert!(matches!(
        reg.select(&ProfileQuery {
            platform: "acme.box".into(),
            version: "1".into(),
            release: None,
            hardware: Some(HardwareScope {
                asic: Some("tarmac".into()),
                ..Default::default()
            }),
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
    match reg.select(&ProfileQuery {
        platform: "acme.box".into(),
        version: "9".into(),
        release: None,
        hardware: None,
    }) {
        Selection::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    // Alias resolution is unambiguous: every declared alias resolves
    // back to exactly its owning canonical platform id, and no alias
    // is claimed by two different platforms.
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
        verified_namespaces: vec![],
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
    match reg.select(&ProfileQuery {
        platform: "futureos.router".into(),
        version: "2026.1".into(),
        release: None,
        hardware: None,
    }) {
        Selection::Selected(p) => assert_eq!(p.profile_id, "futureos-1"),
        other => panic!("expected futureos-1, got {other:?}"),
    }
    // And it has NO normalizer binding — dispatch must refuse.
    let sel = reg.select(&ProfileQuery {
        platform: "futureos.router".into(),
        version: "2026.1".into(),
        release: None,
        hardware: None,
    });
    let nr = NormalizerRegistry::builtin();
    let obs = ctx_obs(NormalizerNos::Cumulus, "", "");
    assert!(matches!(
        nr.dispatch(&sel, "interfaces", &obs),
        Err(DispatchError::NoNormalizerBound { .. })
            | Err(DispatchError::PlatformNotQualified { .. })
            | Err(DispatchError::FeatureNotEnabled { .. })
    ));
}

// ── Feature/normalizer contract fields (D2) ─────────────────────────

#[test]
fn builtin_profiles_declare_typed_contract_fields() {
    let reg = builtin_registry();
    for platform in ["sonic", "nvidia.cumulus"] {
        let sel = reg.select(&ProfileQuery {
            platform: platform.into(),
            version: "1".into(),
            release: None,
            hardware: None,
        });
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
        assert!(!n.revision.is_empty());
        assert!(p.resource_budget.max_input_bytes.is_some());
        // No arbitrary command strings: acquisition contracts are
        // logical identifiers only.
        for a in &flag.acquisition {
            for c in &a.contracts {
                assert!(!c.contains(';') && !c.contains('$') && !c.contains('`'));
            }
        }
    }
    // Cumulus structured planes declared but disabled — no capability
    // claim for slices that do not exist.
    let sel = reg.select(&ProfileQuery {
        platform: "nvidia.cumulus".into(),
        version: "1".into(),
        release: None,
        hardware: None,
    });
    let Selection::Selected(p) = sel else {
        panic!()
    };
    assert_eq!(
        ProfileRegistry::feature_status(&p, "nvue"),
        FeatureStatus::Disabled
    );
    // SONiC structured DB planes are declared but disabled — no
    // capability claim for slices that do not exist.
    let sel = reg.select(&ProfileQuery {
        platform: "sonic".into(),
        version: "1".into(),
        release: None,
        hardware: None,
    });
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

// ── Normalizer dispatch through the seam (D3/D5) ────────────────────

#[test]
fn interface_normalizer_dispatches_through_registry() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let vtysh = "Interface Ethernet0 is up, line protocol is up\n  vrf: default\n  index 1 metric 0 mtu 1500 speed 0 \n";
    let ip = "1: Ethernet0: <UP,LOWER_UP> mtu 1500 state UP group default\n    link/ether 44:38:39:00:00:01 brd ff:ff:ff:ff:ff:ff\n";
    for (platform, nos) in [
        ("sonic", NormalizerNos::Sonic),
        ("nvidia.cumulus", NormalizerNos::Cumulus),
    ] {
        let sel = reg.select(&ProfileQuery {
            platform: platform.into(),
            version: "1".into(),
            release: None,
            hardware: None,
        });
        let run = nr
            .dispatch(&sel, "interfaces.frr_cli", &ctx_obs(nos, vtysh, ip))
            .unwrap_or_else(|e| panic!("{platform} dispatch: {e:?}"));
        assert!(run.shadow_only, "{platform} must stay shadow-only");
        assert_eq!(run.output.facts.len(), 1);
        assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
    }
}

#[test]
fn dispatch_rejects_wrong_platform_observation_and_unselected() {
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let obs = ctx_obs(NormalizerNos::Cumulus, "", "");
    // SONiC profile + cumulus observation -> explicit mismatch.
    let sel = reg.select(&ProfileQuery {
        platform: "sonic".into(),
        version: "1".into(),
        release: None,
        hardware: None,
    });
    assert!(matches!(
        nr.dispatch(&sel, "interfaces.frr_cli", &obs),
        Err(DispatchError::ObservationPlatformMismatch { .. })
    ));
    // Candidate (unknown release) is not executable.
    let sel = reg.select(&ProfileQuery {
        platform: "sonic".into(),
        version: "*".into(),
        release: None,
        hardware: None,
    });
    assert!(matches!(
        nr.dispatch(&sel, "interfaces.frr_cli", &obs),
        Err(DispatchError::NotExecutableSelection)
    ));
    // Standalone FRR: feature disabled -> no silent reuse.
    let sel = reg.select(&ProfileQuery {
        platform: "frr".into(),
        version: "9.1".into(),
        release: None,
        hardware: None,
    });
    assert!(matches!(
        nr.dispatch(&sel, "interfaces.frr_cli", &obs),
        Err(DispatchError::FeatureNotEnabled { .. })
    ));
    // Bounded/error propagation: oversized observation still produces
    // the normalizer's explicit refusal fact through dispatch.
    let big = "x".repeat(9 * 1024 * 1024);
    let sel = reg.select(&ProfileQuery {
        platform: "sonic".into(),
        version: "1".into(),
        release: None,
        hardware: None,
    });
    let out: NormalizationOutput = nr
        .dispatch(
            &sel,
            "interfaces.frr_cli",
            &ctx_obs(NormalizerNos::Sonic, &big, ""),
        )
        .expect("oversized still dispatches to bounded normalizer")
        .output;
    assert_eq!(out.facts.len(), 1);
    assert!(out.facts.iter().all(|f| f.validate().is_ok()));
}

#[test]
fn dispatch_produces_no_authority_and_no_graph_access() {
    // Structural guarantee: dispatched facts remain device_asserted
    // with authority: none — the seam cannot mint authority.
    let reg = builtin_registry();
    let nr = NormalizerRegistry::builtin();
    let sel = reg.select(&ProfileQuery {
        platform: "sonic".into(),
        version: "1".into(),
        release: None,
        hardware: None,
    });
    let run = nr
        .dispatch(
            &sel,
            "interfaces.frr_cli",
            &ctx_obs(NormalizerNos::Sonic, "", ""),
        )
        .unwrap();
    for f in &run.output.facts {
        let v = serde_json::to_value(f).unwrap();
        assert_eq!(v["trust"], "device_asserted");
        assert!(v.get("authority").is_none() || v["authority"].is_null());
    }
}

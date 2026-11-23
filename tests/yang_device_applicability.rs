//! M6C-1 device-effective YANG applicability — T1..T12.
//!
//! Covers the deterministic resolver, RFC 8525 modern/legacy parsing,
//! bounded failure taxonomy, identity separation, durable snapshots,
//! read-only API contract and the no-subscription-regression invariant.
//! Mock gNMI only — no vendor interoperability is claimed.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bonsai::config::{AcquisitionMode, GnmiSecurityMode, TargetConfig};
use bonsai::discovery::DiscoveryInput;
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, GetRequest, GetResponse, ModelData,
    Notification, Path as GnmiPath, PathElem, SubscribeRequest, SubscribeResponse, Update,
    typed_value,
};
use bonsai::sqlite_store::SqliteStore;
use bonsai::yang::YangLibrary;
use bonsai::yang_applicability;
use bonsai::yang_library;
use bonsai::yang_schema::applicability::{
    self, AdvertisedModel, ApplicabilityInput, ApplicabilityStatus, CapabilityEvidence,
    DeviceBinding, ReasonCode, YangLibraryDatastore, YangLibraryEvidence, YangLibraryModule,
    YangLibraryModuleSet, YangLibraryOutcome, YangLibrarySchema, YangLibraryTree,
};
use bonsai::yang_schema::{self, CompileOptions, ModuleInput};
use tokio::sync::{Mutex, watch};
use tonic::{Request, Response, Status, Streaming, transport::Server};

// ── YANG fixtures (inline, deterministic) ────────────────────────────────────

const MODULE_APP: &str = r#"
module test-app {
  yang-version 1.1;
  namespace "urn:example:test-app";
  prefix ta;
  organization "Example Org";
  revision 2024-01-01;

  feature base-f {
    description "base feature";
  }
  feature fancy-f {
    if-feature "ta:base-f";
  }

  container system {
    leaf hostname { type string; config false; }
    leaf domain { type string; }
  }

  list interfaces {
    key "name";
    leaf name { type string; }
    container config {
      leaf mtu { type uint32; units "octets"; if-feature "ta:fancy-f"; }
      leaf admin-status { type string; }
    }
    leaf oper-status { type string; config false; }
  }
}
"#;

const MODULE_BASELIB: &str = r#"
module test-baselib {
  yang-version 1.1;
  namespace "urn:example:test-baselib";
  prefix tbl;
  revision 2023-06-01;

  container platform {
    leaf os-name { type string; config false; }
    leaf version { type string; config false; }
  }
}
"#;

fn module_input(name: &str, yang: &str) -> ModuleInput {
    let module = yang_schema::parse_module(yang).expect("fixture parses");
    ModuleInput {
        module,
        vendor_scope: "test".to_string(),
        source_kind: "inline".to_string(),
        source_ref: name.to_string(),
        trust: "trusted".to_string(),
        checksum_sha256: format!("inline-{}", name),
        approved: false,
        license: String::new(),
        platforms: vec!["*".to_string()],
        pinned_ref: String::new(),
        source_id: String::new(),
    }
}

fn compile_app() -> bonsai::yang_schema::CompiledCatalogue {
    yang_schema::compile_parsed(
        vec![
            module_input("test-app", MODULE_APP),
            module_input("test-baselib", MODULE_BASELIB),
        ],
        &CompileOptions::default(),
    )
}

// ── evidence builders ────────────────────────────────────────────────────────

fn binding() -> DeviceBinding {
    DeviceBinding {
        address: "10.0.0.1:57400".to_string(),
        canonical_address: "10.0.0.1".to_string(),
        external_id: None,
        collector_id: Some("col-local".to_string()),
        identity_ambiguous: false,
    }
}

fn caps_evidence(models: Vec<AdvertisedModel>) -> CapabilityEvidence {
    let mut models = models;
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models.dedup();
    CapabilityEvidence {
        observed_at_ns: 1_000,
        gnmi_version: "0.13.0".to_string(),
        encodings: vec!["JSON_IETF".to_string(), "JSON".to_string()],
        security_mode: "plaintext_explicit".to_string(),
        models,
        collector_id: Some("col-local".to_string()),
    }
}

fn advertised(name: &str, org: &str, version: &str) -> AdvertisedModel {
    AdvertisedModel {
        name: name.to_string(),
        organization: org.to_string(),
        version: version.to_string(),
    }
}

fn lib_module(
    name: &str,
    revision: Option<&str>,
    namespace: Option<&str>,
    features: &[&str],
    deviations: &[&str],
) -> YangLibraryModule {
    YangLibraryModule {
        name: name.to_string(),
        revision: revision.map(str::to_string),
        namespace: namespace.map(str::to_string),
        features: features.iter().map(|s| s.to_string()).collect(),
        deviations: deviations.iter().map(|s| s.to_string()).collect(),
        submodules: Vec::new(),
    }
}

/// Build a complete NMDA library covering `test-app` (revision
/// 2024-01-01, namespace urn:example:test-app) with the given feature
/// set, plus `test-baselib`. Datastore running → schema s1 → ms1.
fn nmda_outcome(
    app_features: &[&str],
    app_deviations: &[&str],
    app_revision: Option<&str>,
) -> YangLibraryOutcome {
    let evidence = YangLibraryEvidence {
        tree: YangLibraryTree::Nmda,
        content_id: Some("cid-1".to_string()),
        module_sets: vec![YangLibraryModuleSet {
            name: "ms1".to_string(),
            modules: vec![
                lib_module(
                    "test-app",
                    app_revision.or(Some("2024-01-01")),
                    Some("urn:example:test-app"),
                    app_features,
                    app_deviations,
                ),
                lib_module(
                    "test-baselib",
                    Some("2023-06-01"),
                    Some("urn:example:test-baselib"),
                    &[],
                    &[],
                ),
            ],
            import_only_modules: Vec::new(),
        }],
        schemas: vec![YangLibrarySchema {
            name: "s1".to_string(),
            module_sets: vec!["ms1".to_string()],
        }],
        datastores: vec![YangLibraryDatastore {
            name: "ietf-datastores:running".to_string(),
            schema: "s1".to_string(),
        }],
        origin: "ietf".to_string(),
        path: "/yang-library".to_string(),
        fetched_at_ns: 2_000,
        diagnostics: Vec::new(),
    };
    YangLibraryOutcome::Complete { evidence }
}

fn resolve<'a>(
    catalogue: Option<&'a bonsai::yang_schema::CompiledCatalogue>,
    caps: Option<&'a CapabilityEvidence>,
    lib: Option<&'a YangLibraryOutcome>,
    device: &'a DeviceBinding,
) -> applicability::ApplicabilityResult {
    applicability::resolve_applicability(&ApplicabilityInput {
        catalogue,
        catalogue_diagnostics: Vec::new(),
        capabilities: caps,
        yang_library: lib,
        requested_datastore: None,
        platform: None,
        device,
        now_ns: 10_000,
    })
}

fn path_status<'a>(
    result: &'a applicability::ApplicabilityResult,
    needle: &str,
) -> Option<&'a applicability::PathResult> {
    result.paths.iter().find(|p| p.path.contains(needle))
}

fn module_status<'a>(
    result: &'a applicability::ApplicabilityResult,
    name: &str,
) -> Option<&'a applicability::ModuleResult> {
    result.modules.iter().find(|m| m.name == name)
}

// ── T1: positive applicability ───────────────────────────────────────────────

#[test]
fn t1_complete_evidence_produces_deterministic_applicable_result() {
    let catalogue = compile_app();
    assert_eq!(catalogue.status, "complete");
    let caps = caps_evidence(vec![advertised("test-app", "example", "9.9")]);
    let lib = nmda_outcome(&["base-f", "fancy-f"], &[], None);
    let device = binding();

    let r1 = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);
    let r2 = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);
    assert_eq!(r1, r2, "resolver must be deterministic");

    assert_eq!(r1.overall_status, ApplicabilityStatus::Applicable);
    assert_eq!(r1.schema_id.as_deref(), Some(catalogue.schema_id.as_str()));
    assert_eq!(r1.content_id.as_deref(), Some("cid-1"));
    assert_eq!(r1.datastore.as_deref(), Some("ietf-datastores:running"));

    let app = module_status(&r1, "test-app").expect("module result");
    assert_eq!(app.status, ApplicabilityStatus::Applicable);
    assert_eq!(app.catalogue_revision, "2024-01-01");
    assert_eq!(app.device_revision.as_deref(), Some("2024-01-01"));

    // Ungated state + config paths are applicable.
    assert_eq!(
        path_status(&r1, "oper-status").unwrap().status,
        ApplicabilityStatus::Applicable
    );
    assert_eq!(
        path_status(&r1, "hostname").unwrap().status,
        ApplicabilityStatus::Applicable
    );
    // fancy-f enabled → gated leaf applicable.
    assert_eq!(
        path_status(&r1, "mtu").unwrap().status,
        ApplicabilityStatus::Applicable
    );
    // Path template stays a template — never an instance key.
    let mtu = path_status(&r1, "mtu").unwrap();
    assert!(
        mtu.path.contains("[name]"),
        "template preserved: {}",
        mtu.path
    );
    // baselib module also resolved.
    assert_eq!(
        module_status(&r1, "test-baselib").unwrap().status,
        ApplicabilityStatus::Applicable
    );
}

// ── T2: capabilities-only evidence ───────────────────────────────────────────

#[test]
fn t2_capabilities_only_never_reaches_applicable() {
    let catalogue = compile_app();
    let caps = caps_evidence(vec![advertised("test-app", "Example Org", "1.0")]);
    let device = binding();
    let r = resolve(Some(&catalogue), Some(&caps), None, &device);

    assert_ne!(r.overall_status, ApplicabilityStatus::Applicable);
    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Candidate);
    assert!(app.reasons.contains(&ReasonCode::CapabilitiesOnlyEvidence));
    assert!(app.reasons.contains(&ReasonCode::ModuleRevisionUnresolved));
    // gNMI model version "1.0" must NOT be treated as a YANG revision.
    assert_ne!(app.device_revision.as_deref(), Some("1.0"));
    for p in &r.paths {
        assert_ne!(p.status, ApplicabilityStatus::Applicable);
    }
}

// ── T3: revision and organization conflicts ──────────────────────────────────

#[test]
fn t3_revision_and_organization_conflicts_stay_unresolved() {
    let catalogue = compile_app();
    let device = binding();

    // Wrong revision → unresolved, not silently matched.
    let lib = nmda_outcome(&["base-f"], &[], Some("1999-12-31"));
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Unknown);
    assert!(app.reasons.contains(&ReasonCode::ModuleRevisionUnresolved));

    // Namespace mismatch → conflicted.
    let mut ev = match lib {
        YangLibraryOutcome::Complete { evidence } => evidence,
        _ => unreachable!(),
    };
    ev.module_sets[0].modules[0].namespace = Some("urn:example:other".to_string());
    let r = resolve(
        Some(&catalogue),
        None,
        Some(&YangLibraryOutcome::Complete { evidence: ev }),
        &device,
    );
    assert_eq!(
        module_status(&r, "test-app").unwrap().status,
        ApplicabilityStatus::Conflicted
    );

    // Capabilities: same name, different organizations → conflict is
    // preserved in evidence; org mismatch vs catalogue → conflicted.
    let caps = caps_evidence(vec![
        advertised("test-app", "vendor-a", "1"),
        advertised("test-app", "vendor-b", "1"),
    ]);
    let r = resolve(Some(&catalogue), Some(&caps), None, &device);
    let app = module_status(&r, "test-app").unwrap();
    // Both orgs recorded in raw evidence; merged evidence keeps first,
    // conflict flag set → conflicted, never silently resolved.
    assert!(caps.models.len() == 2);
    assert!(
        app.status == ApplicabilityStatus::Conflicted
            || app
                .reasons
                .contains(&ReasonCode::ModuleOrganizationConflict)
            || app.status == ApplicabilityStatus::Candidate,
        "org conflict must not produce applicable: {:?} {:?}",
        app.status,
        app.reasons
    );
    assert_ne!(app.status, ApplicabilityStatus::Applicable);
}

/// M6F-D: gNMI Capabilities `organization` is descriptive provenance, not
/// YANG module identity. When RFC 8525 YANG Library membership already
/// proves name+namespace+revision, an organization-string difference is a
/// recorded warning — not a conflict. Capabilities-only evidence keeps the
/// strict conflict behavior (covered by T3).
#[test]
fn t3b_library_backed_organization_mismatch_is_advisory() {
    let catalogue = compile_app();
    let device = binding();
    let caps = caps_evidence(vec![advertised("test-app", "different-org", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);
    let r = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);

    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Applicable);
    assert!(
        app.reasons
            .contains(&ReasonCode::ModuleOrganizationMismatch),
        "org mismatch must be recorded: {:?}",
        app.reasons
    );
    assert!(
        !app.reasons
            .contains(&ReasonCode::ModuleOrganizationConflict),
        "library-backed org mismatch must not be a conflict: {:?}",
        app.reasons
    );
    // Ungated state path remains positively applicable; feature-gated
    // path still obeys feature state.
    assert_eq!(
        path_status(&r, "oper-status").unwrap().status,
        ApplicabilityStatus::Applicable
    );
    assert_eq!(
        path_status(&r, "mtu").unwrap().status,
        ApplicabilityStatus::NotApplicable
    );
}

/// The advisory org-mismatch downgrade must not rescue independently
/// broken identity: a library-backed module whose revision disagrees with
/// the compiled catalogue stays unresolved.
#[test]
fn t3c_org_mismatch_advisory_does_not_override_revision_conflict() {
    let catalogue = compile_app();
    let device = binding();
    let caps = caps_evidence(vec![advertised("test-app", "different-org", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], Some("1999-01-01"));
    let r = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);

    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Unknown);
    assert!(
        app.reasons
            .contains(&ReasonCode::ModuleOrganizationMismatch),
        "org mismatch still recorded: {:?}",
        app.reasons
    );
    assert!(
        app.reasons.contains(&ReasonCode::ModuleRevisionUnresolved),
        "revision conflict must stay explicit: {:?}",
        app.reasons
    );
}

/// Legacy RFC 7895 modules-state establishes module implementation but
/// NOT the datastore→schema link. An org mismatch there is advisory, yet
/// the module can never rise above Candidate.
#[test]
fn t3d_legacy_library_org_mismatch_stays_candidate() {
    let catalogue = compile_app();
    let device = binding();
    let caps = caps_evidence(vec![advertised("test-app", "different-org", "1")]);
    let evidence = YangLibraryEvidence {
        tree: YangLibraryTree::Legacy,
        content_id: None,
        module_sets: vec![YangLibraryModuleSet {
            name: "ms1".to_string(),
            modules: vec![
                lib_module(
                    "test-app",
                    Some("2024-01-01"),
                    Some("urn:example:test-app"),
                    &["base-f"],
                    &[],
                ),
                lib_module(
                    "test-baselib",
                    Some("2023-06-01"),
                    Some("urn:example:test-baselib"),
                    &[],
                    &[],
                ),
            ],
            import_only_modules: Vec::new(),
        }],
        schemas: Vec::new(),
        datastores: Vec::new(),
        origin: "ietf".to_string(),
        path: "/modules-state".to_string(),
        fetched_at_ns: 2_000,
        diagnostics: Vec::new(),
    };
    let lib = YangLibraryOutcome::Legacy { evidence };
    let r = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);

    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Candidate);
    assert!(
        app.reasons
            .contains(&ReasonCode::ModuleOrganizationMismatch),
        "legacy org mismatch is advisory: {:?}",
        app.reasons
    );
    assert!(
        app.reasons.contains(&ReasonCode::DatastoreSchemaUnverified),
        "missing datastore link must stay explicit: {:?}",
        app.reasons
    );
}

// ── T4: features and deviations ──────────────────────────────────────────────

#[test]
fn t4_feature_states_and_deviations_gate_paths() {
    let catalogue = compile_app();
    let device = binding();

    // fancy-f NOT enabled → gated leaf not applicable; base paths fine.
    let lib = nmda_outcome(&["base-f"], &[], None);
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    assert_eq!(
        path_status(&r, "mtu").unwrap().status,
        ApplicabilityStatus::NotApplicable
    );
    assert!(
        path_status(&r, "mtu")
            .unwrap()
            .reasons
            .contains(&ReasonCode::FeatureDisabled)
    );
    assert_eq!(
        path_status(&r, "oper-status").unwrap().status,
        ApplicabilityStatus::Applicable
    );

    // fancy-f enabled but its if-feature base-f absent → expression
    // "fancy-f" is the leaf gate itself; fancy-f enabled requires base-f
    // per YANG semantics? The leaf gate is only fancy-f; base-f absence
    // doesn't disable fancy-f at this layer — but feature state is
    // device-reported: fancy-f enabled while base-f disabled is device
    // evidence we record as-is.
    let lib = nmda_outcome(&["fancy-f"], &[], None);
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    assert_eq!(
        path_status(&r, "mtu").unwrap().status,
        ApplicabilityStatus::Applicable
    );

    // Unknown feature state (caps-only) → never applicable.
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let r = resolve(Some(&catalogue), Some(&caps), None, &device);
    assert!(
        path_status(&r, "mtu")
            .unwrap()
            .reasons
            .contains(&ReasonCode::FeatureStateUnknown)
    );

    // Device-advertised deviation module → deviation semantics
    // unsupported → paths capped at unknown, never applicable.
    let lib = nmda_outcome(&["base-f", "fancy-f"], &["test-app-dev"], None);
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    let app = module_status(&r, "test-app").unwrap();
    assert!(
        app.reasons
            .contains(&ReasonCode::DeviationSemanticsUnsupported)
    );
    for p in r.paths.iter().filter(|p| p.module == "test-app") {
        assert_ne!(p.status, ApplicabilityStatus::Applicable);
        assert!(p.status == ApplicabilityStatus::Unknown);
    }

    // Boolean feature expression evaluation is exercised through the
    // compiler's tri-state evaluator directly.
    use bonsai::yang_schema::{FeatureTruth, eval_feature_expr_tristate};
    let mut enabled = BTreeSet::new();
    enabled.insert("f".to_string());
    assert_eq!(
        eval_feature_expr_tristate("f and not g", &|a| {
            if a == "f" {
                FeatureTruth::True
            } else {
                FeatureTruth::Unknown
            }
        }),
        FeatureTruth::Unknown
    );
    assert_eq!(
        eval_feature_expr_tristate("f or g", &|a| {
            if a == "f" {
                FeatureTruth::True
            } else {
                FeatureTruth::Unknown
            }
        }),
        FeatureTruth::True
    );
    assert_eq!(
        eval_feature_expr_tristate("not f", &|_| FeatureTruth::False),
        FeatureTruth::True
    );
    // Malformed expression → Unknown, never positive.
    assert_eq!(
        eval_feature_expr_tristate("f and )", &|_| FeatureTruth::True),
        FeatureTruth::Unknown
    );
}

// ── T5: import-only and datastore correctness ────────────────────────────────

#[test]
fn t5_import_only_and_datastore_semantics() {
    let catalogue = compile_app();
    let device = binding();

    // Import-only module must not be treated as implemented.
    let mut evidence = match nmda_outcome(&["base-f"], &[], None) {
        YangLibraryOutcome::Complete { evidence } => evidence,
        _ => unreachable!(),
    };
    evidence.module_sets[0].import_only_modules = vec![lib_module(
        "test-baselib",
        Some("2023-06-01"),
        Some("urn:example:test-baselib"),
        &[],
        &[],
    )];
    evidence.module_sets[0]
        .modules
        .retain(|m| m.name != "test-baselib");
    let r = resolve(
        Some(&catalogue),
        None,
        Some(&YangLibraryOutcome::Complete { evidence }),
        &device,
    );
    let bl = module_status(&r, "test-baselib").unwrap();
    assert_eq!(
        bl.classification,
        applicability::ModuleClassification::ImportOnly
    );
    assert_eq!(bl.status, ApplicabilityStatus::NotApplicable);
    assert!(bl.reasons.contains(&ReasonCode::ImportOnlyModule));

    // Datastore mismatch: evaluate against operational datastore →
    // config paths not applicable there.
    let mut ev2 = match nmda_outcome(&["base-f"], &[], None) {
        YangLibraryOutcome::Complete { evidence } => evidence,
        _ => unreachable!(),
    };
    ev2.datastores = vec![
        YangLibraryDatastore {
            name: "ietf-datastores:operational".to_string(),
            schema: "s1".to_string(),
        },
        YangLibraryDatastore {
            name: "ietf-datastores:running".to_string(),
            schema: "s1".to_string(),
        },
    ];
    let r = resolve(
        Some(&catalogue),
        None,
        Some(&YangLibraryOutcome::Complete { evidence: ev2 }),
        &device,
    );
    // Default selection prefers running → still applicable.
    assert_eq!(r.datastore.as_deref(), Some("ietf-datastores:running"));

    // Explicit operational request → config paths mismatch.
    let lib = YangLibraryOutcome::Complete {
        evidence: match nmda_outcome(&["base-f"], &[], None) {
            YangLibraryOutcome::Complete { evidence } => {
                let mut e = evidence;
                e.datastores = vec![YangLibraryDatastore {
                    name: "ietf-datastores:operational".to_string(),
                    schema: "s1".to_string(),
                }];
                e
            }
            _ => unreachable!(),
        },
    };
    let device = binding();
    let result = applicability::resolve_applicability(&ApplicabilityInput {
        catalogue: Some(&catalogue),
        catalogue_diagnostics: Vec::new(),
        capabilities: None,
        yang_library: Some(&lib),
        requested_datastore: Some("operational"),
        platform: None,
        device: &device,
        now_ns: 10_000,
    });
    assert_eq!(
        result.datastore.as_deref(),
        Some("ietf-datastores:operational")
    );
    // config=true leaf (domain) → datastore mismatch.
    let domain = result
        .paths
        .iter()
        .find(|p| p.path.ends_with("/domain"))
        .expect("domain path");
    assert_eq!(domain.status, ApplicabilityStatus::NotApplicable);
    assert!(domain.reasons.contains(&ReasonCode::DatastoreMismatch));
    // state leaf stays applicable.
    let hostname = result
        .paths
        .iter()
        .find(|p| p.path.ends_with("/hostname"))
        .expect("hostname path");
    assert_eq!(hostname.status, ApplicabilityStatus::Applicable);
}

// ── T6: missing or damaged catalogue ─────────────────────────────────────────

#[test]
fn t6_catalogue_failures_never_produce_positive_results() {
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);
    let device = binding();

    // No catalogue at all.
    let r = resolve(None, Some(&caps), Some(&lib), &device);
    assert_eq!(r.overall_status, ApplicabilityStatus::Unknown);
    assert!(r.reasons.contains(&ReasonCode::CatalogueUnavailable));
    assert!(r.modules.is_empty() && r.paths.is_empty());

    // Partial catalogue → capped below applicable.
    let mut partial = compile_app();
    partial.status = "partial".to_string();
    let r = resolve(Some(&partial), Some(&caps), Some(&lib), &device);
    assert_ne!(r.overall_status, ApplicabilityStatus::Applicable);
    assert!(r.reasons.contains(&ReasonCode::CataloguePartial));
    for m in &r.modules {
        assert_eq!(m.status, ApplicabilityStatus::Unknown);
    }

    // Failed / unknown statuses likewise.
    for status in ["failed", "unknown"] {
        let mut c = compile_app();
        c.status = status.to_string();
        let r = resolve(Some(&c), Some(&caps), Some(&lib), &device);
        assert_ne!(r.overall_status, ApplicabilityStatus::Applicable);
    }
}

// ── T7: RFC 8525 parser failure taxonomy ─────────────────────────────────────

#[test]
fn t7_library_parser_bounds_and_outcomes() {
    // Malformed JSON tree → parse error, not panic.
    assert!(
        yang_library::parse_nmda_library(
            &serde_json::json!({"yang-library": {"module-set": "notalist"}}),
            0
        )
        .is_err()
    );

    // Missing container → error.
    assert!(yang_library::parse_nmda_library(&serde_json::json!({"foo": 1}), 0).is_err());

    // Conflicting module definitions across module-sets → error.
    let tree = serde_json::json!({
        "yang-library": {
            "content-id": "x",
            "module-set": [
                {"name": "a", "module": [{"name": "m", "revision": "2020-01-01"}]},
                {"name": "b", "module": [{"name": "m", "revision": "2021-01-01"}]}
            ],
            "schema": [{"name": "s", "module-set": ["a", "b"]}],
            "datastore": [{"name": "ietf-datastores:running", "schema": "s"}]
        }
    });
    let err = yang_library::parse_nmda_library(&tree, 0).unwrap_err();
    assert!(err.contains("conflicting"), "{err}");

    // Oversized module count → bounded error.
    let mut modules = Vec::new();
    for i in 0..(bonsai::yang_schema::applicability::MAX_MODULE_RESULTS as usize + 10) {
        modules.push(serde_json::json!({"name": format!("m{i}"), "revision": "2020-01-01"}));
    }
    let tree = serde_json::json!({
        "yang-library": {
            "module-set": [{"name": "ms", "module": modules}],
            "schema": [{"name": "s", "module-set": ["ms"]}],
            "datastore": [{"name": "ietf-datastores:running", "schema": "s"}]
        }
    });
    assert!(yang_library::parse_nmda_library(&tree, 0).is_err());

    // Legacy modules-state parses into normalized evidence.
    let legacy = serde_json::json!({
        "modules-state": {
            "module-set-id": "msid-1",
            "module": [
                {"name": "test-app", "revision": "2024-01-01",
                 "namespace": "urn:example:test-app",
                 "conformance-type": "implement",
                 "feature": [{"name": "base-f"}]},
                {"name": "ietf-yang-library", "conformance-type": "import"}
            ]
        }
    });
    let ev = yang_library::parse_legacy_library(&legacy, 0).unwrap();
    assert_eq!(ev.tree, YangLibraryTree::Legacy);
    assert_eq!(ev.content_id.as_deref(), Some("msid-1"));
    assert_eq!(ev.module_sets.len(), 1);
    assert_eq!(ev.module_sets[0].modules.len(), 1);
    assert_eq!(ev.module_sets[0].import_only_modules.len(), 1);
    assert!(ev.module_sets[0].modules[0].features.contains("base-f"));

    // Outcome → reason mapping sanity.
    assert_eq!(
        YangLibraryOutcome::Timeout.reason_code(),
        ReasonCode::YangLibraryTimeout
    );
    assert_eq!(
        YangLibraryOutcome::PermissionDenied.reason_code(),
        ReasonCode::YangLibraryPermissionDenied
    );
}

// ── T8: trusted device identity ──────────────────────────────────────────────

#[test]
fn t8_device_identity_binding_isolation() {
    let catalogue = compile_app();
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);

    // Two devices, same catalogue+evidence, different identity →
    // different fingerprints and different persisted device keys.
    let d1 = DeviceBinding {
        address: "10.0.0.1:57400".into(),
        canonical_address: "10.0.0.1".into(),
        external_id: None,
        collector_id: None,
        identity_ambiguous: false,
    };
    let d2 = DeviceBinding {
        address: "10.0.0.2:57400".into(),
        canonical_address: "10.0.0.2".into(),
        external_id: None,
        collector_id: None,
        identity_ambiguous: false,
    };
    let r1 = resolve(Some(&catalogue), Some(&caps), Some(&lib), &d1);
    let r2 = resolve(Some(&catalogue), Some(&caps), Some(&lib), &d2);
    assert_ne!(r1.evidence_fingerprint, r2.evidence_fingerprint);
    assert_eq!(d1.identity_key(), "10.0.0.1");
    assert_eq!(d2.identity_key(), "10.0.0.2");

    // external_id wins as identity key.
    let d3 = DeviceBinding {
        external_id: Some("netbox:device:42".into()),
        ..d1.clone()
    };
    assert_eq!(d3.identity_key(), "netbox:device:42");

    // Ambiguous identity → conflicted, never merged.
    let d4 = DeviceBinding {
        identity_ambiguous: true,
        ..d1.clone()
    };
    let r4 = resolve(Some(&catalogue), Some(&caps), Some(&lib), &d4);
    assert_eq!(r4.overall_status, ApplicabilityStatus::Conflicted);
    assert!(r4.reasons.contains(&ReasonCode::IdentityAmbiguity));
}

// ── T9: persistence and restart ───────────────────────────────────────────────

#[test]
fn t9_snapshot_persistence_restart_and_staleness() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        bonsai::sqlite_store::CURRENT_SCHEMA_VERSION
    );

    let catalogue = compile_app();
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);
    let device = binding();
    let result = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);

    let snap = yang_applicability::ApplicabilitySnapshot {
        snapshot_id: 0,
        device_key: device.identity_key(),
        device_address: device.address.clone(),
        collector_id: device.collector_id.clone(),
        schema_id: result.schema_id.clone(),
        content_id: result.content_id.clone(),
        datastore: result.datastore.clone(),
        overall_status: result.overall_status.as_str().to_string(),
        outcome: "ok".to_string(),
        result,
        capabilities: Some(caps.clone()),
        yang_library: Some(lib.clone()),
        evidence_fingerprint: "fp".to_string(),
        observed_at_ns: Some(5_000),
        evaluated_at_ns: 10_000,
        error: None,
        created_at_ns: 10_000,
        provenance: Default::default(),
    };
    let row = yang_applicability::snapshot_to_row(&snap, 0).unwrap();
    let id = store.save_yang_applicability_snapshot(&row).unwrap();
    assert!(id > 0);

    // "Restart": new store handle on same DB file.
    let store2 = SqliteStore::open(dir.path()).unwrap();
    let loaded = store2
        .latest_yang_applicability_snapshot(&device.identity_key())
        .unwrap()
        .expect("snapshot persisted");
    let decoded = yang_applicability::snapshot_from_row(&loaded).unwrap();
    assert_eq!(decoded.overall_status, "applicable");
    assert_eq!(
        decoded.result.schema_id.as_deref(),
        Some(catalogue.schema_id.as_str())
    );
    assert!(decoded.capabilities.is_some());
    assert!(decoded.yang_library.is_some());

    // Freshness: fresh when schema matches and evidence young.
    let (fresh, reasons) =
        yang_applicability::evaluate_freshness(&decoded, Some(&catalogue.schema_id), 10_000 + 1000);
    assert_eq!(fresh, yang_applicability::SnapshotFreshness::Fresh);
    assert!(reasons.is_empty());

    // Schema change → stale.
    let (fresh, reasons) = yang_applicability::evaluate_freshness(
        &decoded,
        Some("different-schema-id"),
        10_000 + 1000,
    );
    assert_eq!(fresh, yang_applicability::SnapshotFreshness::Stale);
    assert!(reasons.contains(&ReasonCode::SchemaVersionChanged));

    // Evidence expiry → stale.
    let (fresh, reasons) = yang_applicability::evaluate_freshness(
        &decoded,
        Some(&catalogue.schema_id),
        10_000 + yang_applicability::EVIDENCE_MAX_AGE_NS + 1,
    );
    assert_eq!(fresh, yang_applicability::SnapshotFreshness::Stale);
    assert!(reasons.contains(&ReasonCode::EvidenceExpired));

    // History bound: write >16 snapshots, only 16 retained.
    for i in 0..20 {
        let mut s = snap.clone();
        s.evaluated_at_ns = 20_000 + i;
        s.created_at_ns = 20_000 + i;
        let r = yang_applicability::snapshot_to_row(&s, 0).unwrap();
        store2.save_yang_applicability_snapshot(&r).unwrap();
    }
    let history = store2
        .list_yang_applicability_snapshots(&device.identity_key(), 100)
        .unwrap();
    assert_eq!(history.len(), 16);

    // Second device key is isolated.
    let other = store2
        .latest_yang_applicability_snapshot("10.0.0.2")
        .unwrap();
    assert!(other.is_none());
}

// ── mock gNMI server for probe/refresh tests ─────────────────────────────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    get_responses: Arc<Mutex<Vec<GetResponse>>>,
    get_requests: Arc<Mutex<Vec<GetRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl GNmi for MockGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        Ok(Response::new(self.caps.clone()))
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<GetResponse>, Status> {
        self.get_requests.lock().await.push(request.into_inner());
        let mut responses = self.get_responses.lock().await;
        if responses.is_empty() {
            return Ok(Response::new(GetResponse::default()));
        }
        Ok(Response::new(responses.remove(0)))
    }

    type SubscribeStream =
        tokio_stream::wrappers::ReceiverStream<Result<SubscribeResponse, Status>>;

    async fn subscribe(
        &self,
        _request: Request<Streaming<SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn set(
        &self,
        _request: Request<bonsai::proto::gnmi::SetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::SetResponse>, Status> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        Err(Status::unimplemented("set"))
    }
}

struct Lab {
    addr: String,
    get_requests: Arc<Mutex<Vec<GetRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_lab(caps_models: Vec<(&str, &str, &str)>, get_responses: Vec<GetResponse>) -> Lab {
    let get_responses = Arc::new(Mutex::new(get_responses));
    let get_requests = Arc::new(Mutex::new(Vec::new()));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let set_calls = Arc::new(AtomicUsize::new(0));
    let svc = MockGnmi {
        caps: CapabilityResponse {
            supported_models: caps_models
                .into_iter()
                .map(|(name, org, ver)| ModelData {
                    name: name.to_string(),
                    organization: org.to_string(),
                    version: ver.to_string(),
                })
                .collect(),
            supported_encodings: vec![Encoding::JsonIetf as i32],
            g_nmi_version: "0.13.0".to_string(),
            ..Default::default()
        },
        get_responses,
        get_requests: Arc::clone(&get_requests),
        subscribe_calls: Arc::clone(&subscribe_calls),
        set_calls: Arc::clone(&set_calls),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async move {
                    let _ = shutdown_rx.changed().await;
                },
            )
            .await
            .unwrap();
    });
    Lab {
        addr,
        get_requests,
        subscribe_calls,
        set_calls,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

fn lab_target(addr: &str) -> TargetConfig {
    TargetConfig {
        address: addr.to_string(),
        enabled: false,
        security: Some(GnmiSecurityMode::Plaintext),
        collector_id: Some("col-local".to_string()),
        acquisition: AcquisitionMode::Disabled,
        inventory_only: true,
        ..Default::default()
    }
}

fn lab_discovery_input(addr: &str) -> DiscoveryInput {
    DiscoveryInput {
        address: addr.to_string(),
        username: None,
        password: None,
        username_env: None,
        password_env: None,
        ca_cert_path: None,
        tls_domain: None,
        security: Some(GnmiSecurityMode::Plaintext),
        client_cert_path: None,
        client_key_path: None,
        role_hint: None,
        vault: None,
        environment_archetype: None,
    }
}

/// Build a GetResponse carrying a yang-library subtree as one
/// json_ietf_val update (the compact response shape). `content` is the
/// container's content — the update path element names the container.
fn json_get_response(elem_name: &str, content: serde_json::Value) -> GetResponse {
    GetResponse {
        notification: vec![Notification {
            timestamp: 0,
            prefix: None,
            update: vec![Update {
                path: Some(GnmiPath {
                    origin: String::new(),
                    elem: vec![PathElem {
                        name: elem_name.to_string(),
                        key: Default::default(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                val: Some(bonsai::proto::gnmi::TypedValue {
                    value: Some(typed_value::Value::JsonIetfVal(
                        serde_json::to_vec(&content).unwrap(),
                    )),
                }),
                ..Default::default()
            }],
            delete: vec![],
            atomic: false,
        }],
        ..Default::default()
    }
}

/// RFC 8525 NMDA `yang-library` container content (no wrapper — the
/// update path element supplies the container name).
fn nmda_json() -> serde_json::Value {
    serde_json::json!({
        "content-id": "cid-lab",
        "module-set": [{
            "name": "ms1",
            "module": [
                {"name": "test-app", "revision": "2024-01-01",
                 "namespace": "urn:example:test-app",
                 "feature": [{"name": "base-f"}, {"name": "fancy-f"}]},
                {"name": "test-baselib", "revision": "2023-06-01",
                 "namespace": "urn:example:test-baselib"}
            ]
        }],
        "schema": [{"name": "s1", "module-set": ["ms1"]}],
        "datastore": [{"name": "ietf-datastores:running", "schema": "s1"}]
    })
}

// ── T7b/T11: live probe + refresh, no subscription regression ────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t7b_probe_parses_nmda_and_legacy_over_mock_gnmi() {
    // NMDA: first candidate returns full library.
    let lab = start_lab(
        vec![("test-app", "example", "1")],
        vec![json_get_response("yang:yang-library", nmda_json())],
    )
    .await;
    let input = lab_discovery_input(&lab.addr);
    let probe = yang_library::probe_yang_library(&input).await;
    match probe.outcome {
        YangLibraryOutcome::Complete { evidence } => {
            assert_eq!(evidence.content_id.as_deref(), Some("cid-lab"));
            assert_eq!(evidence.tree, YangLibraryTree::Nmda);
            let ms = &evidence.module_sets[0];
            assert_eq!(ms.modules.len(), 2);
            assert!(ms.modules[0].features.contains("base-f"));
        }
        other => panic!("expected Complete, got {}", outcome_debug(&other)),
    }
    assert!(!lab.get_requests.lock().await.is_empty());

    // Empty responses everywhere → Unsupported (caps-only evidence).
    let lab2 = start_lab(vec![("test-app", "example", "1")], vec![]).await;
    let probe2 = yang_library::probe_yang_library(&lab_discovery_input(&lab2.addr)).await;
    assert!(matches!(probe2.outcome, YangLibraryOutcome::Unsupported));

    // Legacy modules-state → Legacy outcome.
    let legacy_json = serde_json::json!({
        "module-set-id": "msid-x",
        "module": [{"name": "test-app", "revision": "2024-01-01",
                    "namespace": "urn:example:test-app",
                    "conformance-type": "implement"}]
    });
    // NMDA candidates return empty (default GetResponse), legacy returns data.
    // NMDA now has 4 candidates × 2 origins = 8 requests before the
    // legacy tree is probed.
    let mut responses = Vec::new();
    for _ in 0..8 {
        responses.push(GetResponse::default());
    }
    responses.push(json_get_response("modules-state", legacy_json));
    let lab3 = start_lab(vec![], responses).await;
    let probe3 = yang_library::probe_yang_library(&lab_discovery_input(&lab3.addr)).await;
    match probe3.outcome {
        YangLibraryOutcome::Legacy { evidence } => {
            assert_eq!(evidence.content_id.as_deref(), Some("msid-x"));
        }
        other => panic!("expected Legacy, got {}", outcome_debug(&other)),
    }
}

fn outcome_debug(o: &YangLibraryOutcome) -> String {
    match o {
        YangLibraryOutcome::Complete { .. } => "complete".into(),
        YangLibraryOutcome::Legacy { .. } => "legacy".into(),
        YangLibraryOutcome::Unsupported => "unsupported".into(),
        YangLibraryOutcome::PermissionDenied => "permission_denied".into(),
        YangLibraryOutcome::Incomplete { reason } => format!("incomplete:{reason}"),
        YangLibraryOutcome::Malformed { reason } => format!("malformed:{reason}"),
        YangLibraryOutcome::Timeout => "timeout".into(),
        YangLibraryOutcome::TransportFailure { reason } => format!("transport:{reason}"),
        YangLibraryOutcome::ConflictingModuleDefinitions { reason } => {
            format!("conflict:{reason}")
        }
        YangLibraryOutcome::UnsupportedEncoding => "unsupported_encoding".into(),
        YangLibraryOutcome::DatastoreSchemaMissing { reason } => {
            format!("ds_missing:{reason}")
        }
        YangLibraryOutcome::DatastoreSchemaAmbiguous { reason } => {
            format!("ds_ambiguous:{reason}")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_refresh_persists_snapshot_and_never_subscribes() {
    // Persist + activate a real compiled catalogue.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let catalogue = compile_app();
    yang_schema::persist_compiled(&library, &catalogue).unwrap();
    yang_schema::activate_compiled(&library, &catalogue.schema_id).unwrap();

    let store = SqliteStore::open(dir.path()).unwrap();
    let lab = start_lab(
        vec![("test-app", "example", "1")],
        vec![json_get_response("yang:yang-library", nmda_json())],
    )
    .await;
    let target = lab_target(&lab.addr);
    let discovery_input = lab_discovery_input(&lab.addr);

    let snapshot =
        yang_applicability::refresh_device_applicability(&yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input,
            identity_ambiguous: false,
            requested_datastore: None,
            store: &store,
        })
        .await
        .expect("refresh succeeds");

    assert_eq!(snapshot.overall_status, "applicable");
    assert_eq!(
        snapshot.schema_id.as_deref(),
        Some(catalogue.schema_id.as_str())
    );
    assert_eq!(snapshot.content_id.as_deref(), Some("cid-lab"));
    assert!(snapshot.capabilities.is_some());
    assert!(matches!(
        snapshot.yang_library,
        Some(YangLibraryOutcome::Complete { .. })
    ));
    assert!(
        snapshot
            .result
            .paths
            .iter()
            .any(|p| p.status == ApplicabilityStatus::Applicable)
    );

    // T11: discovery must not create subscriptions, expand selections,
    // convert disabled acquisition, or issue Set.
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.set_calls.load(Ordering::SeqCst), 0);
    // Only Get RPCs were issued (plus Capabilities).
    assert!(!lab.get_requests.lock().await.is_empty());

    // Snapshot is durable: reload through a fresh store handle.
    let store2 = SqliteStore::open(dir.path()).unwrap();
    let row = store2
        .latest_yang_applicability_snapshot(&snapshot.device_key)
        .unwrap()
        .expect("snapshot row");
    let decoded = yang_applicability::snapshot_from_row(&row).unwrap();
    assert_eq!(decoded.evidence_fingerprint, snapshot.evidence_fingerprint);
    assert_eq!(decoded.result.paths_total, snapshot.result.paths_total);

    // Acquisition config untouched: target still disabled/empty.
    assert_eq!(target.acquisition, AcquisitionMode::Disabled);
    assert!(target.selected_paths.is_empty());
}

// ── T10 helpers: auth rule is covered by unit test in auth.rs; here we
// verify the refresh endpoint contract shape indirectly via handler-level
// types (full HTTP RBAC coverage lives in the auth middleware tests).

#[test]
fn t10_snapshot_payload_contains_no_secret_material() {
    let catalogue = compile_app();
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);
    let device = binding();
    let result = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);
    let snap = yang_applicability::ApplicabilitySnapshot {
        snapshot_id: 1,
        device_key: device.identity_key(),
        device_address: device.address.clone(),
        collector_id: device.collector_id.clone(),
        schema_id: result.schema_id.clone(),
        content_id: result.content_id.clone(),
        datastore: result.datastore.clone(),
        overall_status: result.overall_status.as_str().to_string(),
        outcome: "ok".to_string(),
        result,
        capabilities: Some(caps),
        yang_library: Some(lib),
        evidence_fingerprint: "fp".into(),
        observed_at_ns: Some(1),
        evaluated_at_ns: 2,
        error: None,
        created_at_ns: 2,
        provenance: Default::default(),
    };
    let json = serde_json::to_string(&snap).unwrap();
    for forbidden in ["password", "secret", "token", "private_key", "credential"] {
        assert!(
            !json.contains(forbidden),
            "snapshot JSON must not contain '{forbidden}'"
        );
    }
}

// ── M6F-B: live SR Linux response regression ─────────────────────────────────
//
// Fixtures are sanitized excerpts of the real 2026-11-21 SR Linux capture
// (docs/merger/evidence/2026-11-21-m6f-srlinux): the full module list was
// pruned to representative entries, preserving the exact wire shapes —
// RFC 8525 string leaf-lists for feature/deviation, object submodules,
// module-set/schema/datastore references and content-id. The mock below
// reproduces the observed device behavior: canonical
// `ietf-yang-library:*` and bare container names return the subtree as
// one json_ietf_val update; wrong-namespace aliases fail
// INVALID_ARGUMENT; plain JSON encoding is UNIMPLEMENTED.

const SRLINUX_NMDA_JSON: &str = include_str!("fixtures/yang/srlinux-ietf-yang-library.json");
const SRLINUX_LEGACY_JSON: &str = include_str!("fixtures/yang/srlinux-modules-state.json");

#[derive(Clone)]
struct SrlinuxGnmi {
    get_requests: Arc<Mutex<Vec<GetRequest>>>,
}

fn srlinux_get_response(update_elem: &str, fixture: &str) -> GetResponse {
    GetResponse {
        notification: vec![Notification {
            timestamp: 0,
            prefix: None,
            update: vec![Update {
                path: Some(GnmiPath {
                    origin: String::new(),
                    elem: vec![PathElem {
                        name: update_elem.to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                val: Some(bonsai::proto::gnmi::TypedValue {
                    value: Some(typed_value::Value::JsonIetfVal(fixture.as_bytes().to_vec())),
                }),
                ..Default::default()
            }],
            delete: vec![],
            atomic: false,
        }],
        ..Default::default()
    }
}

#[tonic::async_trait]
impl GNmi for SrlinuxGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        Ok(Response::new(CapabilityResponse {
            supported_models: vec![ModelData {
                name: "ietf-yang-library".to_string(),
                organization: "ietf".to_string(),
                version: "2019-01-04".to_string(),
            }],
            supported_encodings: vec![Encoding::JsonIetf as i32],
            g_nmi_version: "0.10.0".to_string(),
            ..Default::default()
        }))
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<GetResponse>, Status> {
        let req = request.into_inner();
        self.get_requests.lock().await.push(req.clone());
        let Some(path) = req.path.first() else {
            return Ok(Response::new(GetResponse::default()));
        };
        let Some(elem) = path.elem.first() else {
            return Ok(Response::new(GetResponse::default()));
        };
        let name = elem.name.as_str();
        // Live SR Linux: wrong-namespace aliases are rejected before any
        // encoding consideration.
        let fixture = match name {
            "ietf-yang-library:yang-library" | "yang-library" => {
                Some((SRLINUX_NMDA_JSON, "ietf-yang-library:yang-library"))
            }
            "ietf-yang-library:modules-state" | "modules-state" => {
                Some((SRLINUX_LEGACY_JSON, "ietf-yang-library:modules-state"))
            }
            "yang:yang-library"
            | "yang-library:yang-library"
            | "yang:modules-state"
            | "modules-state:modules-state" => {
                return Err(Status::invalid_argument(format!(
                    "Namespace of '{name}' does not match module namespace \
                     'ietf-yang-library'"
                )));
            }
            _ => None,
        };
        let Some((fixture, update_elem)) = fixture else {
            return Ok(Response::new(GetResponse::default()));
        };
        // SR Linux rejects plain JSON encoding on accepted paths.
        if req.encoding == Encoding::Json as i32 {
            return Err(Status::unimplemented(
                "Only ASCII, PROTO/TYPED_VAL, JSON_IETF, JSON_IETF_FROM_ROOT, \
                 PROTO_ANY, PROTO_JSON, PROTO_BYTES and CLI_FLAT encodings \
                 are supported",
            ));
        }
        Ok(Response::new(srlinux_get_response(update_elem, fixture)))
    }

    type SubscribeStream =
        tokio_stream::wrappers::ReceiverStream<Result<SubscribeResponse, Status>>;

    async fn subscribe(
        &self,
        _request: Request<Streaming<SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn set(
        &self,
        _request: Request<bonsai::proto::gnmi::SetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::SetResponse>, Status> {
        Err(Status::unimplemented("set"))
    }
}

async fn start_srlinux_lab() -> (String, Arc<Mutex<Vec<GetRequest>>>, watch::Sender<bool>) {
    let get_requests = Arc::new(Mutex::new(Vec::new()));
    let svc = SrlinuxGnmi {
        get_requests: Arc::clone(&get_requests),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        Server::builder()
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async move {
                    let _ = shutdown_rx.changed().await;
                },
            )
            .await
            .unwrap();
    });
    (addr, get_requests, shutdown_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m6fb_probe_parses_srlinux_nmda_library_over_mock_gnmi() {
    let (addr, get_requests, _shutdown) = start_srlinux_lab().await;
    let input = lab_discovery_input(&addr);
    let probe = yang_library::probe_yang_library(&input).await;
    match probe.outcome {
        YangLibraryOutcome::Complete { evidence } => {
            assert_eq!(evidence.tree, YangLibraryTree::Nmda);
            assert_eq!(evidence.content_id.as_deref(), Some("1lg3lmex1lg3ln8{d1"));
            assert_eq!(evidence.module_sets.len(), 1);
            let ms = &evidence.module_sets[0];
            assert_eq!(ms.name, "nokia");
            // RFC 8525 string leaf-lists: features and deviations must
            // survive parsing (live SR Linux encodes them as strings).
            let netconf = ms
                .modules
                .iter()
                .find(|m| m.name == "ietf-netconf")
                .expect("ietf-netconf module");
            assert_eq!(netconf.revision.as_deref(), Some("2011-06-01"));
            for f in [
                "candidate",
                "confirmed-commit",
                "rollback-on-error",
                "validate",
                "startup",
                "url",
            ] {
                assert!(netconf.features.contains(f), "missing feature {f}");
            }
            assert!(
                netconf
                    .deviations
                    .iter()
                    .any(|d| d == "ietf-srl-deviations")
            );
            // Object-form submodules still parse.
            let aaa = ms
                .modules
                .iter()
                .find(|m| m.name == "openconfig-aaa")
                .expect("openconfig-aaa module");
            assert_eq!(aaa.submodules.len(), 2);
            assert!(aaa.submodules.iter().any(|s| s == "openconfig-aaa-radius"));
            // Import-only modules and datastore/schema references.
            assert_eq!(ms.import_only_modules.len(), 3);
            assert!(
                ms.import_only_modules
                    .iter()
                    .any(|m| m.name == "ietf-datastores")
            );
            assert_eq!(evidence.schemas.len(), 1);
            assert_eq!(evidence.schemas[0].name, "nokia");
            assert_eq!(evidence.datastores.len(), 5);
            assert!(
                evidence
                    .datastores
                    .iter()
                    .any(|d| d.name == "ietf-datastores:running" && d.schema == "nokia")
            );
        }
        other => panic!("expected Complete, got {}", outcome_debug(&other)),
    }

    // The canonical RFC 8525 module-qualified path must be the FIRST
    // Get attempted — not discovered only after alias failures.
    let reqs = get_requests.lock().await;
    assert!(!reqs.is_empty());
    let first = &reqs[0];
    assert_eq!(first.path[0].elem[0].name, "ietf-yang-library:yang-library");
    assert_eq!(first.path[0].origin, "ietf");
    assert_eq!(first.encoding, Encoding::JsonIetf as i32);
    // No Set, no Subscribe RPCs are part of this probe at all.
}

#[test]
fn m6fb_srlinux_fixtures_parse_directly() {
    // NMDA fixture → evidence (parser-level, no transport).
    let nmda: serde_json::Value = serde_json::from_str(SRLINUX_NMDA_JSON).expect("fixture JSON");
    let ev = yang_library::parse_nmda_library(&nmda, 1).expect("nmda parses");
    let netconf = ev.module_sets[0]
        .modules
        .iter()
        .find(|m| m.name == "ietf-netconf")
        .unwrap();
    assert!(netconf.features.contains("url"));
    assert!(
        netconf
            .deviations
            .iter()
            .any(|d| d == "ietf-srl-deviations")
    );

    // Legacy fixture: string features + object deviations (mixed forms).
    let legacy: serde_json::Value =
        serde_json::from_str(SRLINUX_LEGACY_JSON).expect("fixture JSON");
    let lev = yang_library::parse_legacy_library(&legacy, 1).expect("legacy parses");
    assert_eq!(lev.content_id.as_deref(), Some("1lg3lmex1lg3ln8{d1"));
    let ms = &lev.module_sets[0];
    let netconf = ms
        .modules
        .iter()
        .find(|m| m.name == "ietf-netconf")
        .expect("ietf-netconf");
    assert!(netconf.features.contains("confirmed-commit"));
    assert!(
        netconf
            .deviations
            .iter()
            .any(|d| d == "ietf-srl-deviations")
    );
    assert!(
        ms.import_only_modules
            .iter()
            .any(|m| m.name == "iana-if-type")
    );
}

/// Live SR Linux YANG Library probe — opt-in only, never runs in CI.
///
/// ```sh
/// M6F_LIVE_GNMI_ADDR=172.100.102.11:57400 \
/// M6F_LIVE_CA=/path/to/clab-m6f-srlinux/.tls/ca/ca.pem \
/// M6F_LIVE_TLS_DOMAIN=srlinux \
/// M6F_LIVE_USER=admin M6F_LIVE_PASS=... \
/// cargo test -p bonsai --test yang_device_applicability \
///   m6fb_srlinux_live_yang_library_probe -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live NOS probe; requires M6F_LIVE_* env vars"]
async fn m6fb_srlinux_live_yang_library_probe() {
    let addr = std::env::var("M6F_LIVE_GNMI_ADDR").expect("M6F_LIVE_GNMI_ADDR required");
    let input = DiscoveryInput {
        address: addr,
        username: None,
        password: None,
        username_env: Some("M6F_LIVE_USER".to_string()),
        password_env: Some("M6F_LIVE_PASS".to_string()),
        ca_cert_path: std::env::var("M6F_LIVE_CA").ok(),
        tls_domain: std::env::var("M6F_LIVE_TLS_DOMAIN").ok(),
        security: Some(GnmiSecurityMode::TlsVerify),
        client_cert_path: None,
        client_key_path: None,
        role_hint: None,
        vault: None,
        environment_archetype: None,
    };
    let probe = yang_library::probe_yang_library(&input).await;
    eprintln!("attempts: {:#?}", probe.attempts);
    match probe.outcome {
        YangLibraryOutcome::Complete { evidence } => {
            eprintln!(
                "NMDA library: content_id={:?} module_sets={} schemas={} datastores={}",
                evidence.content_id,
                evidence.module_sets.len(),
                evidence.schemas.len(),
                evidence.datastores.len()
            );
            let total_modules: usize = evidence
                .module_sets
                .iter()
                .map(|ms| ms.modules.len() + ms.import_only_modules.len())
                .sum();
            eprintln!("total modules: {total_modules}");
            assert!(total_modules > 0);
        }
        other => panic!("expected Complete, got {}", outcome_debug(&other)),
    }
}

#[test]
fn m6fb_feature_bound_exceeds_live_srlinux_catalog() {
    // Live SR Linux `srl_nokia-features` advertises 620 features; the
    // parser bound must accept real vendor catalogs (regression for the
    // 256 bound that rejected the live device).
    let features: Vec<serde_json::Value> = (0..700)
        .map(|i| serde_json::Value::String(format!("feat-{i}")))
        .collect();
    let tree = serde_json::json!({
        "content-id": "cid-big",
        "module-set": [{
            "name": "ms1",
            "module": [{
                "name": "big-features",
                "revision": "2025-01-01",
                "namespace": "urn:example:big",
                "feature": features
            }]
        }],
        "schema": [{"name": "s1", "module-set": ["ms1"]}],
        "datastore": [{"name": "ietf-datastores:running", "schema": "s1"}]
    });
    let ev = yang_library::parse_nmda_library(&tree, 1).expect("parses");
    assert_eq!(ev.module_sets[0].modules[0].features.len(), 700);
}

// ── T12: vendor release-pin skew (M6F-C Stage D6) ────────────────────────────
//
// A pinned vendor release catalogue must never manufacture applicability
// for a device running a different release, and device-advertised modules
// absent from the compiled catalogue (experimental-only, unpinned source,
// device-internal) must never gain applicability either. These cases model
// the cEOS/EOS skew scenarios: capabilities `ModelData.version` is opaque
// provenance, the YANG Library revision is the only revision authority.

#[test]
fn t12_vendor_release_skew_stays_unresolved() {
    let catalogue = compile_app();
    let device = binding();

    // (a) Release skew: device library reports a plausible adjacent-release
    // revision (e.g. EOS 4.36 catalogue vs 4.37 device). Namespace, org and
    // features all match — revision alone must hold the module unresolved.
    let lib = nmda_outcome(&["base-f"], &[], Some("2023-11-15"));
    let caps = caps_evidence(vec![advertised("test-app", "example", "2023-11-15")]);
    let r = resolve(Some(&catalogue), Some(&caps), Some(&lib), &device);
    let app = module_status(&r, "test-app").unwrap();
    assert_eq!(app.status, ApplicabilityStatus::Unknown);
    assert!(app.reasons.contains(&ReasonCode::ModuleRevisionUnresolved));
    // Overall may still be Applicable via unaffected modules — the skewed
    // module and every path it declares must stay non-applicable.
    assert!(
        r.paths
            .iter()
            .filter(|p| p.module == "test-app")
            .all(|p| p.status != ApplicabilityStatus::Applicable),
        "skewed module paths must not be applicable"
    );

    // (b) Module advertised by capabilities AND present in the device
    // library but absent from the compiled catalogue (experimental-only /
    // unpinned source): it must produce no applicable module result and no
    // applicable path — applicability is catalogue-driven, never
    // device-advertisement-driven.
    let mut ev = match nmda_outcome(&["base-f"], &[], None) {
        YangLibraryOutcome::Complete { evidence } => evidence,
        _ => unreachable!(),
    };
    ev.module_sets[0].modules.push(lib_module(
        "arista-exp-foo",
        Some("2025-01-01"),
        Some("urn:arista:exp"),
        &[],
        &[],
    ));
    let caps = caps_evidence(vec![
        advertised("test-app", "example", "9.9"),
        advertised("arista-exp-foo", "arista", "4.37.0F"),
    ]);
    let r = resolve(
        Some(&catalogue),
        Some(&caps),
        Some(&YangLibraryOutcome::Complete { evidence: ev }),
        &device,
    );
    assert!(module_status(&r, "arista-exp-foo").is_none());
    assert!(r.paths.iter().all(|p| !p.path.contains("arista-exp")));
    // The catalogue modules that DO have complete evidence still resolve —
    // an uncatalogued device module does not poison the whole snapshot.
    assert_eq!(
        module_status(&r, "test-app").unwrap().status,
        ApplicabilityStatus::Applicable
    );

    // (c) Requested datastore absent from the device library (XRd declares
    // only `running`/`candiate`; a plan asking for `operational` must fail
    // closed, never fall back silently).
    let lib = nmda_outcome(&["base-f"], &[], None);
    let r = applicability::resolve_applicability(&ApplicabilityInput {
        catalogue: Some(&catalogue),
        catalogue_diagnostics: Vec::new(),
        capabilities: None,
        yang_library: Some(&lib),
        requested_datastore: Some("ietf-datastores:operational"),
        platform: None,
        device: &device,
        now_ns: 10_000,
    });
    assert_ne!(r.overall_status, ApplicabilityStatus::Applicable);
    assert!(
        r.reasons.contains(&ReasonCode::DatastoreSchemaMissing)
            || r.reasons.contains(&ReasonCode::DatastoreMismatch)
            || r.modules
                .iter()
                .any(|m| m.reasons.contains(&ReasonCode::DatastoreSchemaMissing)
                    || m.reasons.contains(&ReasonCode::DatastoreMismatch)),
        "missing requested datastore must stay explicit: {:?}",
        r.reasons
    );
}

// ── M6F-F: per-device schema selection ──────────────────────────────────────

fn selection(
    key: &str,
    addr: &str,
    schema_id: &str,
) -> bonsai::sqlite_store::DeviceSchemaSelection {
    bonsai::sqlite_store::DeviceSchemaSelection {
        device_key: key.to_string(),
        device_address: addr.to_string(),
        schema_id: schema_id.to_string(),
        selection_source: "admin".to_string(),
        selected_by: Some("tester".to_string()),
        note: None,
        selection_revision: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    }
}

fn compile_baselib_only() -> bonsai::yang_schema::CompiledCatalogue {
    yang_schema::compile_parsed(
        vec![module_input("test-baselib", MODULE_BASELIB)],
        &CompileOptions::default(),
    )
}

#[test]
fn m6ff_selection_store_crud_restart_references() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    assert!(
        store
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .is_none()
    );

    let sel = selection(
        "10.0.0.9",
        "10.0.0.9:57400",
        "ysc-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let stored = store
        .set_device_schema_selection(&sel, "admin", "test")
        .unwrap();
    assert_eq!(stored.selection_revision, 1);
    // Idempotent same-provenance upsert keeps the selection epoch stable.
    let again = store
        .set_device_schema_selection(&sel, "admin", "test")
        .unwrap();
    assert_eq!(again.selection_revision, 1);
    // Any provenance/content change bumps the epoch.
    let mut changed = sel.clone();
    changed.note = Some("operator note".to_string());
    let bumped = store
        .set_device_schema_selection(&changed, "admin", "test")
        .unwrap();
    assert_eq!(bumped.selection_revision, 2);
    assert!(
        store
            .compiled_schema_references()
            .unwrap()
            .contains(&sel.schema_id)
    );

    // Restart: a fresh store handle sees the durable row.
    let store2 = SqliteStore::open(dir.path()).unwrap();
    let loaded = store2
        .get_device_schema_selection("10.0.0.9")
        .unwrap()
        .expect("selection persisted");
    assert_eq!(loaded.schema_id, sel.schema_id);
    assert_eq!(loaded.selection_revision, 2);
    assert_eq!(store2.list_device_schema_selections().unwrap().len(), 1);

    // Clear → default-catalogue fallback, references released.
    assert!(
        store2
            .clear_device_schema_selection("10.0.0.9", "admin", "test")
            .unwrap()
    );
    assert!(
        store2
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .is_none()
    );
    assert!(
        !store2
            .compiled_schema_references()
            .unwrap()
            .contains(&sel.schema_id)
    );
    assert!(
        !store2
            .clear_device_schema_selection("10.0.0.9", "admin", "test")
            .unwrap()
    );
}

#[test]
fn m6ff_effective_catalogue_selection_isolation_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cat_a = compile_app();
    let cat_b = compile_baselib_only();
    assert_ne!(cat_a.schema_id, cat_b.schema_id);
    yang_schema::persist_compiled(&library, &cat_a).unwrap();
    yang_schema::persist_compiled(&library, &cat_b).unwrap();
    yang_schema::activate_compiled(&library, &cat_a.schema_id).unwrap();
    let store = SqliteStore::open(store_dir.path()).unwrap();

    // No selection row → the globally active catalogue, exactly as before.
    let eff =
        yang_applicability::resolve_effective_catalogue(&library, &store, "10.0.0.9").unwrap();
    assert_eq!(
        eff.effective_schema_id.as_deref(),
        Some(cat_a.schema_id.as_str())
    );
    assert!(eff.selection.is_none());
    assert_eq!(eff.catalogue.as_ref().unwrap().schema_id, cat_a.schema_id);

    // Explicit selection binds ONLY this device to cat_b.
    store
        .set_device_schema_selection(
            &selection("10.0.0.9", "10.0.0.9:57400", &cat_b.schema_id),
            "admin",
            "test",
        )
        .unwrap();
    let eff =
        yang_applicability::resolve_effective_catalogue(&library, &store, "10.0.0.9").unwrap();
    assert_eq!(eff.catalogue.as_ref().unwrap().schema_id, cat_b.schema_id);
    assert_eq!(
        eff.effective_schema_id.as_deref(),
        Some(cat_b.schema_id.as_str())
    );
    assert_eq!(
        eff.active_schema_id.as_deref(),
        Some(cat_a.schema_id.as_str())
    );
    assert_eq!(eff.selection.as_ref().unwrap().schema_id, cat_b.schema_id);
    // No cross-device authority: a sibling device still uses cat_a.
    let other =
        yang_applicability::resolve_effective_catalogue(&library, &store, "10.0.0.10").unwrap();
    assert_eq!(other.catalogue.as_ref().unwrap().schema_id, cat_a.schema_id);

    // A selection naming a missing bundle fails closed — never a silent
    // fallback to the global catalogue.
    let ghost = "ysc-00000000000000000000000000000000";
    store
        .set_device_schema_selection(
            &selection("10.0.0.11", "10.0.0.11:57400", ghost),
            "admin",
            "test",
        )
        .unwrap();
    let eff =
        yang_applicability::resolve_effective_catalogue(&library, &store, "10.0.0.11").unwrap();
    assert!(eff.catalogue.is_none());
    assert!(eff.catalogue_state.is_some());
    assert_eq!(eff.effective_schema_id.as_deref(), Some(ghost));

    // Clearing the selection restores the default catalogue.
    store
        .clear_device_schema_selection("10.0.0.9", "admin", "test")
        .unwrap();
    let eff =
        yang_applicability::resolve_effective_catalogue(&library, &store, "10.0.0.9").unwrap();
    assert_eq!(eff.catalogue.as_ref().unwrap().schema_id, cat_a.schema_id);
    assert!(eff.selection.is_none());
}

#[test]
fn m6ff_snapshot_freshness_binds_to_effective_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cat_a = compile_app();
    let cat_b = compile_baselib_only();
    yang_schema::persist_compiled(&library, &cat_a).unwrap();
    yang_schema::persist_compiled(&library, &cat_b).unwrap();
    yang_schema::activate_compiled(&library, &cat_a.schema_id).unwrap();
    let store = SqliteStore::open(store_dir.path()).unwrap();

    // Snapshot observed under cat_a (the then-active catalogue).
    let device = DeviceBinding {
        address: "10.0.0.9:57400".to_string(),
        canonical_address: "10.0.0.9".to_string(),
        external_id: None,
        collector_id: Some("col-local".to_string()),
        identity_ambiguous: false,
    };
    let caps = caps_evidence(vec![advertised("test-app", "example", "1")]);
    let lib = nmda_outcome(&["base-f"], &[], None);
    let result = resolve(Some(&cat_a), Some(&caps), Some(&lib), &device);
    let snap = yang_applicability::ApplicabilitySnapshot {
        snapshot_id: 0,
        device_key: device.identity_key(),
        device_address: device.address.clone(),
        collector_id: device.collector_id.clone(),
        schema_id: result.schema_id.clone(),
        content_id: result.content_id.clone(),
        datastore: result.datastore.clone(),
        overall_status: result.overall_status.as_str().to_string(),
        outcome: "ok".to_string(),
        result,
        capabilities: Some(caps),
        yang_library: Some(lib),
        evidence_fingerprint: "fp".to_string(),
        observed_at_ns: Some(15_000),
        evaluated_at_ns: 20_000,
        error: None,
        created_at_ns: 20_000,
        provenance: Default::default(),
    };
    let row = yang_applicability::snapshot_to_row(&snap, 0).unwrap();
    store.save_yang_applicability_snapshot(&row).unwrap();

    let target = lab_target("10.0.0.9:57400");
    let now = 20_000 + 1000;

    // Default: fresh against the active catalogue.
    let view = yang_applicability::load_snapshot_view(&store, &library, &target, now)
        .unwrap()
        .expect("snapshot view");
    assert_eq!(view.freshness, yang_applicability::SnapshotFreshness::Fresh);
    assert_eq!(
        view.effective_schema_id.as_deref(),
        Some(cat_a.schema_id.as_str())
    );

    // Selecting cat_b makes THIS device's cat_a-bound snapshot stale —
    // freshness compares against the device-effective schema, not the
    // global pointer.
    store
        .set_device_schema_selection(
            &selection("10.0.0.9", "10.0.0.9:57400", &cat_b.schema_id),
            "admin",
            "test",
        )
        .unwrap();
    let view = yang_applicability::load_snapshot_view(&store, &library, &target, now)
        .unwrap()
        .expect("snapshot view");
    assert_eq!(view.freshness, yang_applicability::SnapshotFreshness::Stale);
    assert!(
        view.freshness_reasons
            .contains(&ReasonCode::SchemaVersionChanged)
    );
    assert_eq!(
        view.effective_schema_id.as_deref(),
        Some(cat_b.schema_id.as_str())
    );
    assert_eq!(
        view.selected_schema_id.as_deref(),
        Some(cat_b.schema_id.as_str())
    );
    assert_eq!(
        view.active_schema_id.as_deref(),
        Some(cat_a.schema_id.as_str())
    );

    // Re-selecting cat_a restores freshness deterministically — the
    // durable selection, not approval state, governs.
    store
        .set_device_schema_selection(
            &selection("10.0.0.9", "10.0.0.9:57400", &cat_a.schema_id),
            "admin",
            "test",
        )
        .unwrap();
    let view = yang_applicability::load_snapshot_view(&store, &library, &target, now)
        .unwrap()
        .expect("snapshot view");
    assert_eq!(view.freshness, yang_applicability::SnapshotFreshness::Fresh);

    // A selection whose bundle cannot be verified stays an explicit
    // catalogue state — never silently fresh.
    store
        .set_device_schema_selection(
            &selection(
                "10.0.0.9",
                "10.0.0.9:57400",
                "ysc-00000000000000000000000000000000",
            ),
            "admin",
            "test",
        )
        .unwrap();
    let view = yang_applicability::load_snapshot_view(&store, &library, &target, now)
        .unwrap()
        .expect("snapshot view");
    assert_eq!(view.freshness, yang_applicability::SnapshotFreshness::Stale);
    assert!(view.catalogue_state.is_some());
}

// ── M6F-F PR3: covered vs uncovered device-reported deviations ─────────

const MODULE_DEV: &str = r#"
module test-app-dev {
  yang-version 1.1;
  namespace "urn:example:test-app-dev";
  prefix tad;
  revision 2024-02-01;
  import test-app { prefix ta; }
  deviation "/ta:system/hostname" {
    deviate delete {
      default "";
    }
  }
}
"#;

#[test]
fn t4b_covered_deviation_does_not_block_applicability() {
    // The device reports a deviation module that IS compiled into the
    // catalogue and fully applied — deviation semantics are evaluated,
    // so paths are not capped at unknown. An uncompiled reported
    // deviation still fails closed.
    let catalogue = yang_schema::compile_parsed(
        vec![
            module_input("test-app", MODULE_APP),
            module_input("test-baselib", MODULE_BASELIB),
            module_input("test-app-dev", MODULE_DEV),
        ],
        &CompileOptions::default(),
    );
    assert_eq!(
        catalogue.status, "complete",
        "diags={:?}",
        catalogue.diagnostics
    );
    let device = binding();

    // Covered: reported deviation == applied deviation module.
    let lib = nmda_outcome(&["base-f", "fancy-f"], &["test-app-dev"], None);
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    let app = module_status(&r, "test-app").unwrap();
    assert!(
        !app.reasons
            .contains(&ReasonCode::DeviationSemanticsUnsupported),
        "covered deviation must not be flagged unsupported: {:?}",
        app.reasons
    );
    assert_eq!(
        path_status(&r, "oper-status").unwrap().status,
        ApplicabilityStatus::Applicable
    );
    // The deviated node itself is pruned from the effective schema.
    assert!(
        r.paths
            .iter()
            .filter(|p| p.module == "test-app")
            .all(|p| !p.path.contains("hostname")),
        "deviate delete must remove the node from compiled paths"
    );

    // Uncovered: a DIFFERENT reported deviation was never compiled.
    let lib = nmda_outcome(&["base-f", "fancy-f"], &["test-app-dev", "other-dev"], None);
    let r = resolve(Some(&catalogue), None, Some(&lib), &device);
    let app = module_status(&r, "test-app").unwrap();
    assert!(
        app.reasons
            .contains(&ReasonCode::DeviationSemanticsUnsupported),
        "uncovered reported deviation must fail closed"
    );
}

// ── M6G: schema-selection optimistic concurrency ────────────────────────

#[test]
fn m6g_selection_fencing_rejects_stale_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();

    // No row: expected 0 is the only valid precondition.
    let sel = selection(
        "10.0.0.9",
        "10.0.0.9:57400",
        "ysc-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let stored = store
        .set_device_schema_selection_fenced(&sel, 0, "admin-a", "test")
        .unwrap();
    assert_eq!(stored.selection_revision, 1);

    // Stale precondition (expected 0 while rev 1 exists) fails closed.
    let mut sel_b = sel.clone();
    sel_b.schema_id = "ysc-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    let err = store
        .set_device_schema_selection_fenced(&sel_b, 0, "admin-b", "test")
        .unwrap_err();
    assert!(
        err.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
            .is_some(),
        "expected SelectionRevisionConflict, got {err:#}"
    );
    // The stored selection is untouched — no silent last-writer-wins.
    assert_eq!(
        store
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .unwrap()
            .schema_id,
        sel.schema_id
    );

    // Correct precondition (rev 1) commits and bumps to rev 2.
    let stored = store
        .set_device_schema_selection_fenced(&sel_b, 1, "admin-b", "test")
        .unwrap();
    assert_eq!(stored.selection_revision, 2);

    // A second stale writer fenced on rev 1 now conflicts.
    let mut sel_c = sel.clone();
    sel_c.schema_id = "ysc-cccccccccccccccccccccccccccccccc".to_string();
    let err = store
        .set_device_schema_selection_fenced(&sel_c, 1, "admin-c", "test")
        .unwrap_err();
    assert!(
        err.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
            .is_some()
    );
}

#[test]
fn m6g_selection_fencing_concurrent_writers_single_winner() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(SqliteStore::open(dir.path()).unwrap());
    let sel = selection(
        "10.0.0.9",
        "10.0.0.9:57400",
        "ysc-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    store
        .set_device_schema_selection_fenced(&sel, 0, "admin", "test")
        .unwrap();

    // Two admins race the same expected revision — exactly one commits.
    let mut handles = Vec::new();
    for (i, sid) in ["d", "e"].iter().enumerate() {
        let store = store.clone();
        let mut s = sel.clone();
        s.schema_id = format!("ysc-{}", sid.repeat(32));
        handles.push(std::thread::spawn(move || {
            store.set_device_schema_selection_fenced(&s, 1, "racer", "test")
        }));
        let _ = i;
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let wins = results.iter().filter(|r| r.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|r| {
            r.as_ref().err().is_some_and(|e| {
                e.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
                    .is_some()
            })
        })
        .count();
    assert_eq!(wins, 1, "exactly one racer may commit: {results:?}");
    assert_eq!(conflicts, 1, "the loser must see a revision conflict");
    assert_eq!(
        store
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .unwrap()
            .selection_revision,
        2
    );
}

#[test]
fn m6g_selection_fenced_clear() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let sel = selection(
        "10.0.0.9",
        "10.0.0.9:57400",
        "ysc-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    store
        .set_device_schema_selection_fenced(&sel, 0, "admin", "test")
        .unwrap();

    // Stale clear (expected 0) conflicts; correct revision deletes.
    let err = store
        .clear_device_schema_selection_fenced("10.0.0.9", 0, "admin", "test")
        .unwrap_err();
    assert!(
        err.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
            .is_some()
    );
    assert!(
        store
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .is_some()
    );

    assert!(
        store
            .clear_device_schema_selection_fenced("10.0.0.9", 1, "admin", "test")
            .unwrap()
    );
    assert!(
        store
            .get_device_schema_selection("10.0.0.9")
            .unwrap()
            .is_none()
    );

    // Clearing an absent selection expects the CURRENT durable epoch
    // (2 after the clear — it does not reset to 0) and reports no row
    // deleted (false), while a wrong precondition still conflicts.
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 2);
    assert!(
        !store
            .clear_device_schema_selection_fenced("10.0.0.9", 2, "admin", "test")
            .unwrap()
    );
    let err = store
        .clear_device_schema_selection_fenced("10.0.0.9", 3, "admin", "test")
        .unwrap_err();
    assert!(
        err.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
            .is_some()
    );
}

#[test]
fn m6g_selection_epoch_is_durable_across_clear() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let sel = selection("10.0.0.9", "10.0.0.9:57400", "ysc-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Never mutated → epoch 0.
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 0);

    // Unfenced internal write still advances the durable epoch.
    store
        .set_device_schema_selection(&sel, "admin", "test")
        .unwrap();
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 1);

    // Idempotent resubmit does not burn an epoch.
    store
        .set_device_schema_selection(&sel, "admin", "test")
        .unwrap();
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 1);

    // Clear deletes the row but the epoch advances and survives.
    assert!(store.clear_device_schema_selection("10.0.0.9", "admin", "test").unwrap());
    assert!(store.get_device_schema_selection("10.0.0.9").unwrap().is_none());
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 2);

    // ABA: expected=0 no longer matches — the stale pre-clear client
    // cannot overwrite post-clear state.
    let err = store
        .set_device_schema_selection_fenced(&sel, 0, "stale", "test")
        .unwrap_err();
    assert!(
        err.downcast_ref::<bonsai::sqlite_store::SelectionRevisionConflict>()
            .is_some()
    );

    // Reselect lands on epoch 3, never resetting to 1.
    let stored = store
        .set_device_schema_selection_fenced(&sel, 2, "admin", "test")
        .unwrap();
    assert_eq!(stored.selection_revision, 3);
    assert_eq!(store.selection_epoch("10.0.0.9").unwrap(), 3);
}

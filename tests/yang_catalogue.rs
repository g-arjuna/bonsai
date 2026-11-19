//! M6 effective YANG catalogue — source manifest, content-addressed import,
//! pinned sync, bundle integrity, effective-schema compilation and the
//! canonical schema-path index. All fixtures are local; no network access.

use std::path::{Path, PathBuf};

use bonsai::yang::{
    YangImportOptions, YangLibrary, compute_bundle_id, select_active_records, trust_rank,
};
use bonsai::yang_schema::{
    CompileOptions, ModuleInput, SchemaPathQuery, activate_compiled, compile_library,
    compile_parsed, filter_by_features, list_compiled, load_active_catalogue, search_active_paths,
};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yang")
}

fn module_input(name: &str) -> ModuleInput {
    let path = fixture_dir().join(format!("{}.yang", name));
    let raw = std::fs::read_to_string(&path).expect("fixture readable");
    let module = bonsai::yang_schema::parse_module(&raw).expect("fixture parses");
    ModuleInput {
        module,
        vendor_scope: "test".to_string(),
        source_kind: "fixture".to_string(),
        source_ref: path.display().to_string(),
        trust: "trusted".to_string(),
        checksum_sha256: format!("fixture-{}", name),
    }
}

fn compile_fixtures() -> bonsai::yang_schema::CompiledCatalogue {
    compile_parsed(
        vec![
            module_input("test-base"),
            module_input("test-sub"),
            module_input("test-aug"),
            module_input("test-deviated"),
            module_input("test-broken"),
        ],
        &CompileOptions::default(),
    )
}

fn find_path<'a>(
    catalogue: &'a bonsai::yang_schema::CompiledCatalogue,
    path: &str,
) -> Option<&'a bonsai::yang_schema::SchemaPathEntry> {
    catalogue.paths.iter().find(|p| p.path == path)
}

// ---------------------------------------------------------------------------
// M6A — import / storage / conflicts
// ---------------------------------------------------------------------------

#[test]
fn import_stores_content_addressed_immutable_modules() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let report = library
        .import_directory(
            &fixture_dir(),
            &YangImportOptions {
                source_kind: "test".to_string(),
                source_ref: "fixture".to_string(),
                vendor_scope: "test".to_string(),
                trust: "trusted".to_string(),
                license: "test-license".to_string(),
                platforms: vec!["test_platform".to_string()],
                pinned_ref: "abc123".to_string(),
                filename_prefixes: Vec::new(),
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert!(report.imported >= 4, "imported={}", report.imported);

    let state = library.load_state().unwrap();
    let base = state
        .modules
        .iter()
        .find(|m| m.module_name == "test-base")
        .expect("test-base imported");
    // Content-addressed blob path.
    assert!(base.relative_path.contains("/blobs/"));
    assert!(
        base.relative_path
            .ends_with(&format!("{}.yang", base.checksum_sha256))
    );
    assert_eq!(base.license, "test-license");
    assert_eq!(base.platforms, vec!["test_platform".to_string()]);
    assert_eq!(base.pinned_ref, "abc123");
    // Blob content matches checksum.
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(dir.path().join("lib").join(&base.relative_path)).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    assert_eq!(digest, base.checksum_sha256);
}

#[test]
fn conflicting_same_name_revision_never_overwrites_approved_content() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();

    // First import: module "dup" revision "1".
    let src1 = dir.path().join("src1");
    std::fs::create_dir_all(&src1).unwrap();
    std::fs::write(
        src1.join("dup.yang"),
        "module dup {\n  yang-version \"1\";\n  namespace \"urn:x:dup\";\n  prefix d;\n  revision \"2024-01-01\";\n  container a { leaf x { type string; } }\n}\n",
    )
    .unwrap();
    library
        .import_directory(
            &src1,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "one".into(),
                vendor_scope: "v".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();

    // Second import: same name+revision, DIFFERENT content.
    let src2 = dir.path().join("src2");
    std::fs::create_dir_all(&src2).unwrap();
    std::fs::write(
        src2.join("dup.yang"),
        "module dup {\n  yang-version \"1\";\n  namespace \"urn:x:dup\";\n  prefix d;\n  revision \"2024-01-01\";\n  container b { leaf y { type uint32; } }\n}\n",
    )
    .unwrap();
    let report = library
        .import_directory(
            &src2,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "two".into(),
                vendor_scope: "v".into(),
                trust: "experimental".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert_eq!(report.imported, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("conflicting content"))
    );

    let state = library.load_state().unwrap();
    let dups: Vec<_> = state
        .modules
        .iter()
        .filter(|m| m.module_name == "dup" && m.revision == "2024-01-01")
        .collect();
    assert_eq!(dups.len(), 2, "both contents must be retained");
    assert_ne!(dups[0].checksum_sha256, dups[1].checksum_sha256);

    // Active selection: trusted record wins over experimental regardless of
    // import order — previously approved content is not displaced.
    let active = library.active_modules().unwrap();
    let winner = active.iter().find(|m| m.module_name == "dup").unwrap();
    assert_eq!(winner.trust, "trusted");
    let trusted_rec = dups.iter().find(|m| m.trust == "trusted").unwrap();
    assert_eq!(winner.checksum_sha256, trusted_rec.checksum_sha256);

    // Both blobs still on disk.
    for d in &dups {
        assert!(dir.path().join("lib").join(&d.relative_path).exists());
    }
}

#[test]
fn active_selection_is_deterministic() {
    // trust_rank ordering sanity.
    assert!(trust_rank("trusted") > trust_rank("experimental"));
    assert!(trust_rank("experimental") > trust_rank("bogus"));
    assert!(trust_rank("") == 0);

    // select_active_records picks one winner per name+revision.
    let mk =
        |name: &str, rev: &str, trust: &str, ts: i64, sum: &str| bonsai::yang::YangModuleRecord {
            module_name: name.into(),
            revision: rev.into(),
            namespace: String::new(),
            organization: String::new(),
            source_kind: "t".into(),
            source_ref: "t".into(),
            vendor_scope: "v".into(),
            trust: trust.into(),
            license: String::new(),
            platforms: vec![],
            pinned_ref: String::new(),
            relative_path: format!("modules/blobs/{}.yang", sum),
            checksum_sha256: sum.into(),
            imported_at_ns: ts,
        };
    let records = vec![
        mk("m", "1", "experimental", 200, "bbb"),
        mk("m", "1", "trusted", 100, "aaa"),
        mk("m", "1", "trusted", 300, "ccc"),
        mk("other", "1", "trusted", 1, "ddd"),
    ];
    let active = select_active_records(&records);
    assert_eq!(active.len(), 2);
    let m = active.iter().find(|r| r.module_name == "m").unwrap();
    // Same trust → newest import wins (ccc @300), deterministically.
    assert_eq!(m.checksum_sha256, "ccc");
}

#[test]
fn oversized_and_filtered_files_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Oversized file (>8MiB).
    let big = "module big { ".to_string() + &"x".repeat(9 * 1024 * 1024);
    std::fs::write(src.join("big.yang"), big).unwrap();
    std::fs::write(
        src.join("keep.yang"),
        "module keep {\n  yang-version \"1\";\n  namespace \"urn:x:k\";\n  prefix k;\n  revision \"2024-01-01\";\n}\n",
    )
    .unwrap();
    std::fs::write(src.join("skip-me.yang"), "module skipme { }").unwrap();
    std::fs::write(src.join("notes.txt"), "not yang").unwrap();

    let report = library
        .import_directory(
            &src,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "t".into(),
                vendor_scope: "v".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec!["keep".into()],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    // Only keep.yang matches the prefix; big/skip-me/notes filtered.
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped, 2); // big.yang + skip-me.yang; notes.txt is not .yang

    // Without prefix filter: oversized file warned+skipped, txt ignored.
    let report2 = library
        .import_directory(
            &src,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "t".into(),
                vendor_scope: "v".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert!(report2.warnings.iter().any(|w| w.contains("oversized")));
    let state = library.load_state().unwrap();
    assert!(state.modules.iter().any(|m| m.module_name == "keep"));
    assert!(state.modules.iter().any(|m| m.module_name == "skipme"));
    assert!(!state.modules.iter().any(|m| m.module_name == "big"));
}

#[test]
fn pinned_git_sync_is_reproducible_and_offline() {
    let dir = tempfile::tempdir().unwrap();
    // Build a local git "remote" with one pinned commit.
    let remote = dir.path().join("remote");
    std::fs::create_dir_all(remote.join("models")).unwrap();
    std::fs::write(
        remote.join("models/pin.yang"),
        "module pin {\n  yang-version \"1\";\n  namespace \"urn:x:pin\";\n  prefix p;\n  revision \"2024-05-05\";\n  container c { leaf l { type string; } }\n}\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&remote)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    let sha = git(&["rev-parse", "HEAD"]);
    git(&["tag", "pinned-v1"]);
    // Move HEAD forward with a second commit — sync must NOT pick this up.
    std::fs::write(
        remote.join("models/pin.yang"),
        "module pin {\n  yang-version \"1\";\n  namespace \"urn:x:pin\";\n  prefix p;\n  revision \"2024-05-05\";\n  container c { leaf l { type string; } leaf extra { type uint32; } }\n}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "second",
    ]);

    // Manifest pointing at the local repo via file:// (allowed for tests).
    let manifest_path = dir.path().join("sources.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"schema_version = 1
[[source]]
id = "pin-src"
vendor = "testpin"
repo = "file://{}"
pinned_ref = "pinned-v1"
platforms = ["test_platform"]
subdirs = ["models"]
license = "test"
enabled = true
"#,
            remote.display(),
        ),
    )
    .unwrap();

    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let report = library
        .sync_from_manifest(
            Some(&manifest_path),
            None,
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert_eq!(report.sources.len(), 1);
    // source_ref records repo@resolved-commit (the tag's target commit).
    assert!(report.sources[0].contains("@"));
    assert!(report.sources[0].ends_with(&sha));

    let state = library.load_state().unwrap();
    let pin = state
        .modules
        .iter()
        .find(|m| m.module_name == "pin")
        .expect("pin module imported");
    assert_eq!(pin.pinned_ref, "pinned-v1");
    assert_eq!(
        pin.source_ref,
        format!("file://{}@{}", remote.display(), sha)
    );
    // Content is the FIRST commit (no `extra` leaf).
    let content = std::fs::read_to_string(dir.path().join("lib").join(&pin.relative_path)).unwrap();
    assert!(!content.contains("extra"));

    // Re-sync is idempotent.
    let report2 = library
        .sync_from_manifest(
            Some(&manifest_path),
            None,
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert_eq!(report2.imported, 0);
    assert_eq!(report2.skipped, 1);
}

#[test]
fn production_manifest_remains_https_only() {
    // file:// is permitted for operator-managed local mirrors and tests;
    // the checked-in production manifest must still be https-only.
    let manifest =
        bonsai::yang_source::load_manifest(Path::new("config/yang_sources.toml")).unwrap();
    for s in &manifest.source {
        assert!(s.repo.starts_with("https://"), "{}", s.id);
        assert!(s.pinned_ref.len() == 40 || !s.pinned_ref.is_empty());
    }
}

#[test]
fn bundle_roundtrip_verifies_content_identity() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(
        dir.path().join("lib"),
        dir.path().join("cache"),
        "TEST_BUNDLE_KEY",
    )
    .unwrap();
    // SAFETY: test-only env mutation; no other thread in this test binary
    // reads TEST_BUNDLE_KEY concurrently.
    unsafe { std::env::set_var("TEST_BUNDLE_KEY", "secret") };
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("b.yang"),
        "module b {\n  yang-version \"1\";\n  namespace \"urn:x:b\";\n  prefix b;\n  revision \"2024-01-01\";\n  container c { leaf l { type string; } }\n}\n",
    )
    .unwrap();
    library
        .import_directory(
            &src,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "t".into(),
                vendor_scope: "vb".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();

    let bundle_path = dir.path().join("bundle.tar");
    let manifest = library.create_bundle("vb", None, &bundle_path).unwrap();
    assert!(manifest.bundle_id.starts_with("bundle-"));
    // Recompute independently.
    let expected = compute_bundle_id(&manifest.modules);
    assert_eq!(manifest.bundle_id, expected);

    // Fresh library installs the bundle.
    let library2 = YangLibrary::open(
        dir.path().join("lib2"),
        dir.path().join("cache2"),
        "TEST_BUNDLE_KEY",
    )
    .unwrap();
    let report = library2
        .install_bundle(&bundle_path, Path::new("config/path_profiles"))
        .unwrap();
    assert_eq!(report.imported, 1);
    let state = library2.load_state().unwrap();
    assert!(state.modules.iter().any(|m| m.module_name == "b"));

    // Tampered manifest → bundle_id mismatch rejected before import.
    // (Rebuild a tar with modified manifest but same signature is impossible;
    //  instead verify compute_bundle_id detects module-list changes.)
    let mut tampered = manifest.modules.clone();
    tampered[0].checksum_sha256 = "deadbeef".to_string();
    assert_ne!(compute_bundle_id(&tampered), manifest.bundle_id);
}

// ---------------------------------------------------------------------------
// M6B — effective schema compilation + path index
// ---------------------------------------------------------------------------

#[test]
fn compiles_paths_with_keys_types_units_and_config() {
    let catalogue = compile_fixtures();
    // Only test-broken is expected to produce diagnostics.
    assert!(
        catalogue
            .diagnostics
            .iter()
            .all(|d| d.contains("test-broken")),
        "{:?}",
        catalogue.diagnostics
    );

    // Multi-key list: canonical path carries both keys on the list entry.
    let users = find_path(&catalogue, "/users[name][domain]").expect("users list");
    assert_eq!(users.kind, "list");
    assert_eq!(users.keys, vec!["name".to_string(), "domain".to_string()]);
    let uid = find_path(&catalogue, "/users[name][domain]/uid").expect("uid path");
    assert_eq!(uid.kind, "leaf");
    assert!(uid.mandatory);
    assert_eq!(uid.units.as_deref(), Some("id"));
    assert!(uid.config);

    // Typedef resolution: percent → uint8.
    let util = find_path(&catalogue, "/policed/utilization").expect("utilization");
    assert_eq!(util.type_name.as_deref(), Some("percent"));
    assert_eq!(util.resolved_type.as_deref(), Some("uint8"));
    assert!(!util.config, "state leaf under uses-expanded container");

    // uses/grouping expansion + units on leaf.
    let mtu = find_path(&catalogue, "/policed/mtu").expect("mtu via grouping");
    assert_eq!(mtu.units.as_deref(), Some("octets"));
    assert!(!mtu.config);

    // config=false inheritance: system subtree is state-only.
    let ntp = find_path(&catalogue, "/system/ntp/server").expect("ntp server");
    assert!(!ntp.config);

    // Choice/case are transparent in canonical paths.
    let fast = find_path(&catalogue, "/ports/fast-mbps").expect("fast-mbps");
    assert!(find_path(&catalogue, "/ports/speed/fast/fast-mbps").is_none());
    assert!(find_path(&catalogue, "/ports/speed").is_none());
    // if-feature recorded.
    assert!(fast.required_features.iter().any(|f| f == "tb:fast-mode"));

    // Submodule merge: sub-only/marker resolves sub-typedef → uint16.
    let marker = find_path(&catalogue, "/sub-only/marker").expect("submodule node");
    assert_eq!(marker.resolved_type.as_deref(), Some("uint16"));
    assert!(!marker.config);
}

#[test]
fn augments_and_deviations_apply_with_provenance() {
    let catalogue = compile_fixtures();

    // Augmented nodes appear under the target tree.
    let tz = find_path(&catalogue, "/system/timezone").expect("augmented timezone");
    assert_eq!(tz.module, "test-aug", "augment provenance");
    assert_eq!(tz.units.as_deref(), Some("tz"));
    let flag = find_path(&catalogue, "/system/extra/flag").expect("augmented flag");
    assert_eq!(flag.default.as_deref(), Some("\"false\""));
    let note = find_path(&catalogue, "/ports/admin-note").expect("augmented note");
    assert_eq!(note.module, "test-aug");

    // Deviation on /tb:ports/speed marks the whole choice subtree.
    let fast = find_path(&catalogue, "/ports/fast-mbps").unwrap();
    assert!(fast.deviated);
    assert!(fast.deviation_notes.iter().any(|n| n.contains("deviated")));

    // Deviation on leaf marks it + status obsolete.
    let server = find_path(&catalogue, "/system/ntp/server").unwrap();
    assert!(server.deviated);
    assert_eq!(server.status.as_deref(), Some("obsolete"));
}

#[test]
fn missing_dependencies_are_diagnosed_not_fatal() {
    let catalogue = compile_fixtures();
    // test-broken imports a nonexistent module and uses an unknown typedef.
    assert!(
        catalogue
            .diagnostics
            .iter()
            .any(|d| d.contains("nonexistent-module") || d.contains("not a builtin"))
    );
    // The broken module's own nodes are still indexed (partial resolution).
    let orphan = find_path(&catalogue, "/orphan/x");
    assert!(orphan.is_some());
    // And its type stayed raw/unresolved rather than fabricated.
    let broken_mod = catalogue
        .modules
        .iter()
        .find(|m| m.name == "test-broken")
        .unwrap();
    assert_eq!(broken_mod.resolution, "partial");
}

#[test]
fn feature_filtering_is_explicit() {
    let catalogue = compile_fixtures();
    let mut enabled = std::collections::BTreeSet::new();
    // Without the feature, fast-mbps is still indexed but filtered out.
    let all = filter_by_features(&catalogue.paths, &enabled);
    assert!(all.iter().all(|p| p.path != "/ports/fast-mbps"));
    enabled.insert("tb:fast-mode".to_string());
    let with_feat = filter_by_features(&catalogue.paths, &enabled);
    assert!(with_feat.iter().any(|p| p.path == "/ports/fast-mbps"));
}

#[test]
fn compile_persist_activate_search_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for name in ["test-base", "test-sub", "test-aug"] {
        let raw = std::fs::read_to_string(fixture_dir().join(format!("{}.yang", name))).unwrap();
        std::fs::write(src.join(format!("{}.yang", name)), raw).unwrap();
    }
    library
        .import_directory(
            &src,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "t".into(),
                vendor_scope: "v".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();

    // Compile v1 and activate.
    let c1 = compile_library(&library, true).unwrap();
    activate_compiled(&library, &c1.schema_id).unwrap();

    // Search active paths.
    let res = search_active_paths(
        &library,
        &SchemaPathQuery {
            query: "fast-mbps".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(res.schema_id.as_deref(), Some(c1.schema_id.as_str()));
    assert!(res.paths.iter().any(|p| p.path == "/ports/fast-mbps"));

    // State-only filter.
    let res_state = search_active_paths(
        &library,
        &SchemaPathQuery {
            query: String::new(),
            config: Some(false),
            limit: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(res_state.paths.iter().all(|p| !p.config));
    assert!(
        res_state
            .paths
            .iter()
            .any(|p| p.path == "/system/ntp/server")
    );

    // Compile a second catalogue (different content) → distinct schema_id.
    std::fs::write(
        src.join("test-base.yang"),
        std::fs::read_to_string(fixture_dir().join("test-base.yang"))
            .unwrap()
            .replace("interface MTU", "interface MTU v2"),
    )
    .unwrap();
    library
        .import_directory(
            &src,
            &YangImportOptions {
                source_kind: "test".into(),
                source_ref: "t2".into(),
                vendor_scope: "v".into(),
                trust: "trusted".into(),
                license: String::new(),
                platforms: vec![],
                pinned_ref: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    let c2 = compile_library(&library, true).unwrap();
    assert_ne!(c1.schema_id, c2.schema_id);

    // Active is still c1 until activated; rollback = reactivate c1.
    let active = load_active_catalogue(&library).unwrap().unwrap();
    assert_eq!(active.schema_id, c1.schema_id);
    activate_compiled(&library, &c2.schema_id).unwrap();
    let active2 = load_active_catalogue(&library).unwrap().unwrap();
    assert_eq!(active2.schema_id, c2.schema_id);
    activate_compiled(&library, &c1.schema_id).unwrap();
    let rolled = load_active_catalogue(&library).unwrap().unwrap();
    assert_eq!(rolled.schema_id, c1.schema_id);

    // Both bundles retained and listed.
    let bundles = list_compiled(&library).unwrap();
    assert!(bundles.len() >= 2);
    assert!(bundles.iter().filter(|b| b.active).count() == 1);
}

#[test]
fn parser_rejects_malformed_without_panic() {
    for bad in [
        "module {",
        "module m { container c { leaf l { type } } }",
        "not a yang file at all {{{",
        "module m { revision \"2024\" { description \"x\"; } }",
        "",
    ] {
        // Must not panic; error or partial AST are both acceptable.
        let _ = bonsai::yang_schema::parse_module(bad);
    }
}

#[test]
fn parses_and_compiles_real_openconfig_models() {
    // Pinned OpenConfig public release v5.11.0 fixtures (Apache-2.0) plus
    // pinned IETF RFC modules from YangModels/yang@e13b05c. Together they
    // cover the full import closure of openconfig-interfaces except for
    // modules that genuinely have no telemetry data-defs.
    let fixture_root = fixture_dir();
    let mut inputs = Vec::new();
    for sub in ["oc", "ietf"] {
        for entry in std::fs::read_dir(fixture_root.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("yang") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).unwrap();
            let module = bonsai::yang_schema::parse_module(&raw)
                .unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
            inputs.push(ModuleInput {
                module,
                vendor_scope: "openconfig".into(),
                source_kind: "fixture".into(),
                source_ref: path.display().to_string(),
                trust: "trusted".into(),
                checksum_sha256: String::new(),
            });
        }
    }
    assert_eq!(inputs.len(), 9);
    let catalogue = compile_parsed(inputs, &CompileOptions::default());

    // Independent oracle comparison: the committed reference was generated
    // by `scripts/yang_reference.py` (pyang -f tree) from these exact
    // fixtures — see tests/fixtures/yang/MANIFEST.md. We compare the full
    // effective data-node set per module: path, kind, config flag,
    // mandatory flag (leaves only) and list keys.
    let reference: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("reference/expected-schema.json"))
            .expect("reference schema readable"),
    )
    .expect("reference schema JSON");

    // Bonsai path -> normalized comparison key (list keys stripped).
    fn norm_path(p: &str) -> String {
        let mut out = String::with_capacity(p.len());
        let mut chars = p.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '[' {
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    let mut missing: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    let mut mismatched: Vec<String> = Vec::new();
    let ref_modules = reference["modules"].as_object().expect("modules object");

    // Every module pyang reports data nodes for must appear in Bonsai.
    for (module, entries) in ref_modules {
        let actual: std::collections::BTreeMap<String, &bonsai::yang_schema::SchemaPathEntry> =
            catalogue
                .paths
                .iter()
                .filter(|p| &p.module == module)
                .map(|p| (norm_path(&p.path), p))
                .collect();
        let expected: std::collections::BTreeMap<String, &serde_json::Value> = entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["path"].as_str().unwrap().to_string(), e))
            .collect();
        for (path, exp) in &expected {
            match actual.get(path) {
                None => missing.push(format!("{}/{}", module, path)),
                Some(act) => {
                    let mut diffs = Vec::new();
                    if act.kind != exp["kind"].as_str().unwrap() {
                        diffs.push(format!(
                            "kind bonsai={} pyang={}",
                            act.kind,
                            exp["kind"].as_str().unwrap()
                        ));
                    }
                    if act.config != exp["config"].as_bool().unwrap() {
                        diffs.push(format!(
                            "config bonsai={} pyang={}",
                            act.config,
                            exp["config"].as_bool().unwrap()
                        ));
                    }
                    if let Some(pm) = exp["mandatory"].as_bool() {
                        if act.mandatory != pm {
                            diffs.push(format!("mandatory bonsai={} pyang={}", act.mandatory, pm));
                        }
                    }
                    let exp_keys: Vec<String> = exp["keys"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|k| k.as_str().unwrap().to_string())
                        .collect();
                    if act.keys != exp_keys {
                        diffs.push(format!("keys bonsai={:?} pyang={:?}", act.keys, exp_keys));
                    }
                    let exp_status = exp["status"].as_str().unwrap_or("current");
                    let act_status = act.status.as_deref().unwrap_or("current");
                    if act_status != exp_status {
                        diffs.push(format!("status bonsai={} pyang={}", act_status, exp_status));
                    }
                    if !diffs.is_empty() {
                        mismatched.push(format!("{}/{}: {}", module, path, diffs.join("; ")));
                    }
                }
            }
        }
        for path in actual.keys() {
            if !expected.contains_key(path) {
                extra.push(format!("{}/{}", module, path));
            }
        }
    }
    // Bonsai paths in modules the oracle has no data tree for are extras.
    for p in &catalogue.paths {
        if !ref_modules.contains_key(&p.module) {
            extra.push(format!("{}/{} (module not in oracle)", p.module, p.path));
        }
    }

    assert!(
        missing.is_empty() && extra.is_empty() && mismatched.is_empty(),
        "schema divergence from pyang reference:\nmissing={} extra={} mismatched={}\nmissing: {:?}\nextra: {:?}\nmismatched: {:?}",
        missing.len(),
        extra.len(),
        mismatched.len(),
        missing.iter().take(20).collect::<Vec<_>>(),
        extra.iter().take(20).collect::<Vec<_>>(),
        mismatched.iter().take(20).collect::<Vec<_>>()
    );

    // Canonical OC interface list exists with its key.
    let iface = catalogue
        .paths
        .iter()
        .find(|p| p.path == "/interfaces/interface[name]")
        .expect("interface list path");
    assert_eq!(iface.kind, "list");
    assert_eq!(iface.keys, vec!["name".to_string()]);
    // The fixture set covers the import closure: no module may be reported
    // missing, and unresolved references must surface as diagnostics.
    assert!(
        !catalogue
            .diagnostics
            .iter()
            .any(|d| d.contains("is not present in the compiled set"))
    );
}

// ---------------------------------------------------------------------------
// Lexer / string semantics (RFC 7950 §6.1.3)
// ---------------------------------------------------------------------------

fn inline_input(name: &str, yang: &str) -> ModuleInput {
    ModuleInput {
        module: bonsai::yang_schema::parse_module(yang).expect("inline module parses"),
        vendor_scope: "test".into(),
        source_kind: "fixture".into(),
        source_ref: name.into(),
        trust: "trusted".into(),
        checksum_sha256: String::new(),
    }
}

#[test]
fn string_lexing_follows_rfc7950_613() {
    let yang = r#"module strtest {
  yang-version 1.1;
  namespace "urn:t:str";
  prefix st;
  container c {
    leaf sq { type string; description 'C:\new\dir\file'; }
    leaf dq { type string; description "tab\there \"q\" \\ end"; }
    leaf bad { type string; description "keep\q literal"; }
    leaf mix { type string; description "foo" + 'bar' + "baz"; }
    leaf multi { type string; description "line1
        line2"; }
    leaf uni { type string; description "héllo→"; }
    leaf empty { type string; description ""; }
  }
}
"#;
    let catalogue = compile_parsed(
        vec![inline_input("strtest", yang)],
        &CompileOptions::default(),
    );
    assert!(
        catalogue.diagnostics.is_empty(),
        "{:?}",
        catalogue.diagnostics
    );

    let desc = |p: &str| {
        find_path(&catalogue, p)
            .unwrap_or_else(|| panic!("missing {}", p))
            .description
            .clone()
    };
    // Single-quoted: backslashes are literal — \n stays two chars.
    assert_eq!(desc("/c/sq").as_deref(), Some(r"C:\new\dir\file"));
    // Double-quoted: valid escapes processed.
    assert_eq!(desc("/c/dq").as_deref(), Some("tab\there \"q\" \\ end"));
    // Unknown double-quoted escape preserved literally (pyang-compatible).
    assert_eq!(desc("/c/bad").as_deref(), Some(r"keep\q literal"));
    // Mixed single/double concatenation.
    assert_eq!(desc("/c/mix").as_deref(), Some("foobarbaz"));
    // Multiline double-quoted folds to a single space.
    assert_eq!(desc("/c/multi").as_deref(), Some("line1 line2"));
    // UTF-8 preserved.
    assert_eq!(desc("/c/uni").as_deref(), Some("héllo→"));
    // Empty string is legal.
    assert_eq!(desc("/c/empty").as_deref(), Some(""));
}

#[test]
fn unterminated_and_malformed_strings_fail_closed() {
    for bad in [
        "module x { yang-version 1.1; namespace \"urn:x\"; prefix x; container c { leaf a { type string; description \"abc; } } }",
        "module x { yang-version 1.1; namespace \"urn:x\"; prefix x; container c { leaf a { type string; description 'abc; } } }",
        "module x { yang-version 1.1; namespace \"urn:x",
    ] {
        assert!(bonsai::yang_schema::parse_module(bad).is_err(), "{}", bad);
    }
}

// ---------------------------------------------------------------------------
// Resolution hardening
// ---------------------------------------------------------------------------

#[test]
fn circular_grouping_uses_is_bounded_not_fatal() {
    let yang = r#"module cyc {
  yang-version 1.1;
  namespace "urn:t:cyc";
  prefix cy;
  grouping loop {
    uses loop;
    leaf marker { type string; }
  }
  uses loop;
}
"#;
    let catalogue = compile_parsed(vec![inline_input("cyc", yang)], &CompileOptions::default());
    // Bounded expansion emits an explicit diagnostic, never hangs/crashes.
    assert!(
        catalogue
            .diagnostics
            .iter()
            .any(|d| d.contains("depth exceeded") || d.contains("circular"))
    );
}

#[test]
fn uses_if_feature_gates_expanded_subtree() {
    let yang = r#"module featgate {
  yang-version 1.1;
  namespace "urn:t:fg";
  prefix fg;
  grouping gated {
    leaf inner { type string; }
    container deep { leaf deeper { type string; } }
  }
  uses gated {
    if-feature fg:some-feature;
  }
}
"#;
    let catalogue = compile_parsed(
        vec![inline_input("featgate", yang)],
        &CompileOptions::default(),
    );
    for p in ["/inner", "/deep", "/deep/deeper"] {
        let e = find_path(&catalogue, p).unwrap_or_else(|| panic!("missing {}", p));
        assert!(
            e.required_features.iter().any(|f| f == "fg:some-feature"),
            "{} missing feature gate: {:?}",
            p,
            e.required_features
        );
    }
}

#[test]
fn compile_is_deterministic_under_input_reordering() {
    let opts = CompileOptions::default();
    let names = ["test-base", "test-sub", "test-aug", "test-deviated"];
    let c1 = compile_parsed(names.iter().map(|n| module_input(n)).collect(), &opts);
    let reversed: Vec<&str> = names.iter().rev().copied().collect();
    let c2 = compile_parsed(reversed.iter().map(|n| module_input(n)).collect(), &opts);
    assert_eq!(c1.schema_id, c2.schema_id);
    assert_eq!(
        serde_json::to_string(&c1.paths).unwrap(),
        serde_json::to_string(&c2.paths).unwrap()
    );
    assert_eq!(c1.status, c2.status);
}

#[test]
fn catalogue_status_distinguishes_complete_partial_failed() {
    // Fixture set has an intentionally broken module -> partial.
    assert_eq!(compile_fixtures().status, "partial");
    // Clean subset without test-broken -> complete.
    let clean = compile_parsed(
        vec![module_input("test-base"), module_input("test-sub")],
        &CompileOptions::default(),
    );
    assert_eq!(clean.status, "complete", "diags={:?}", clean.diagnostics);
    // No inputs -> failed.
    let empty = compile_parsed(Vec::new(), &CompileOptions::default());
    assert_eq!(empty.status, "failed");
}

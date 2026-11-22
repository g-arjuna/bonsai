//! M6 effective YANG catalogue — source manifest, content-addressed import,
//! pinned sync, bundle integrity, effective-schema compilation and the
//! canonical schema-path index. All fixtures are local; no network access.

use std::path::{Path, PathBuf};

use bonsai::yang::{
    YangImportOptions, YangLibrary, compute_bundle_id, select_active_records, trust_rank,
};
use bonsai::yang_schema::{
    CompileOptions, ModuleInput, SchemaPathQuery, activate_compiled, compile_library,
    compile_parsed, filter_by_features, list_compiled, load_active_catalogue, persist_compiled,
    search_active_paths,
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
        approved: false,
        license: String::new(),
        platforms: Vec::new(),
        pinned_ref: String::new(),
        source_id: String::new(),
    }
}

fn inline_input(name: &str, yang: &str) -> ModuleInput {
    let module = bonsai::yang_schema::parse_module(yang).expect("inline module parses");
    ModuleInput {
        module,
        vendor_scope: "test".to_string(),
        source_kind: "inline".to_string(),
        source_ref: name.to_string(),
        trust: "trusted".to_string(),
        checksum_sha256: format!("inline-{}", name),
        approved: false,
        license: String::new(),
        platforms: Vec::new(),
        pinned_ref: String::new(),
        source_id: String::new(),
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
                source_id: String::new(),
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
                source_id: String::new(),
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
                source_id: String::new(),
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
            source_id: String::new(),
            approved: false,
            relative_path: format!("modules/blobs/{}.yang", sum),
            checksum_sha256: sum.into(),
            imported_at_ns: ts,
        };
    // Incumbent stability: the earliest-imported equal-trust record wins
    // even when a LATER import has the lower checksum — checksum order
    // must never silently displace previously selected content.
    let records = vec![
        mk("m", "1", "experimental", 200, "bbb"),
        mk("m", "1", "trusted", 100, "zzz"), // incumbent, HIGHER checksum
        mk("m", "1", "trusted", 300, "aaa"), // later import, LOWER checksum
        mk("other", "1", "trusted", 1, "ddd"),
    ];
    let active = select_active_records(&records);
    assert_eq!(active.len(), 2);
    let m = active.iter().find(|r| r.module_name == "m").unwrap();
    assert_eq!(m.checksum_sha256, "zzz");
    assert_eq!(m.imported_at_ns, 100);
}

#[test]
fn incumbent_survives_lower_checksum_challenger() {
    // Exact review scenario: incumbent checksum sorts HIGHER than the
    // newly imported conflicting checksum. Incumbent must remain selected
    // until an explicit checksum-specific approval promotes the challenger.
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
            source_id: String::new(),
            approved: false,
            relative_path: format!("modules/blobs/{}.yang", sum),
            checksum_sha256: sum.into(),
            imported_at_ns: ts,
        };
    let records = vec![
        mk("m", "1", "trusted", 100, "zzz"), // incumbent, higher checksum
        mk("m", "1", "trusted", 200, "aaa"), // challenger, lower checksum
    ];
    let active = select_active_records(&records);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].checksum_sha256, "zzz");
    assert_eq!(active[0].imported_at_ns, 100);
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
                source_id: String::new(),
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
                source_id: String::new(),
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
    // Tags are moving references and are rejected by manifest validation;
    // the pin must be the commit SHA itself.
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
pinned_ref = "{sha}"
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
    // pinned_ref is the immutable commit SHA from the manifest (tags and
    // branches are rejected by validation).
    assert_eq!(pin.pinned_ref, sha);
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
                source_id: String::new(),
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
                source_id: String::new(),
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
    // Conflicting same name+revision content never displaces the incumbent
    // implicitly (stable-checksum selection); promotion requires approval.
    let before = library.load_state().unwrap();
    let incumbent = before
        .modules
        .iter()
        .find(|m| m.module_name == "test-base")
        .unwrap()
        .checksum_sha256
        .clone();
    let report2 = library
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    assert!(report2.warnings.iter().any(|w| w.contains("conflicting")));
    let challenger = library
        .load_state()
        .unwrap()
        .modules
        .into_iter()
        .find(|m| m.module_name == "test-base" && m.checksum_sha256 != incumbent)
        .expect("conflicting record stored");
    library
        .approve_module(
            "test-base",
            &challenger.revision,
            &challenger.checksum_sha256,
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
                approved: false,
                license: String::new(),
                platforms: Vec::new(),
                pinned_ref: String::new(),
                source_id: String::new(),
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

// ---------------------------------------------------------------------------
// Corrective review regressions (A/B/C/D findings)
// ---------------------------------------------------------------------------

#[test]
fn schema_identity_binds_semantics_not_just_names() {
    let opts = CompileOptions::default();
    let base = compile_parsed(vec![module_input("test-base")], &opts);
    // Same input → same identity.
    let again = compile_parsed(vec![module_input("test-base")], &opts);
    assert_eq!(base.schema_id, again.schema_id);
    // Adding a module changes identity.
    let more = compile_parsed(
        vec![module_input("test-base"), module_input("test-sub")],
        &opts,
    );
    assert_ne!(base.schema_id, more.schema_id);
    // A semantic change inside the module changes identity.
    let changed_yang = r#"module solo {
  yang-version "1.1";
  namespace "urn:t:solo";
  prefix s;
  revision "2024-01-01";
  container c { leaf x { type string; } }
}
"#;
    let c1 = compile_parsed(vec![inline_input("solo", changed_yang)], &opts);
    let c2_yang = changed_yang.replace("leaf x { type string; }", "leaf x { type uint32; }");
    let c2 = compile_parsed(vec![inline_input("solo", &c2_yang)], &opts);
    assert_ne!(
        c1.schema_id, c2.schema_id,
        "type change must change identity"
    );
    // Diagnostics participate: a pre-diagnostic changes status AND identity.
    let with_diag = bonsai::yang_schema::compile_with_diagnostics(
        vec![inline_input("solo", changed_yang)],
        &opts,
        vec!["pre-existing failure".to_string()],
    );
    assert_eq!(with_diag.status, "partial");
    assert_ne!(c1.schema_id, with_diag.schema_id);
}

#[test]
fn pre_compilation_diagnostics_never_yield_complete() {
    // An unreadable/corrupt module record surfaces as a pre-diagnostic and
    // must downgrade the catalogue — never silently `complete`.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("good.yang"),
        "module good { yang-version \"1\"; namespace \"urn:g\"; prefix g; revision \"2024-01-01\"; container c { leaf x { type string; } } }\n",
    )
    .unwrap();
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    // Corrupt the stored blob: record/content mismatch must be diagnosed.
    let state = library.load_state().unwrap();
    let rel = &state.modules[0].relative_path;
    let blob = dir.path().join("lib").join(rel);
    std::fs::write(&blob, "module different { }\n").unwrap();
    let catalogue = compile_library(&library, true).unwrap();
    assert_ne!(
        catalogue.status, "complete",
        "diags={:?}",
        catalogue.diagnostics
    );
    assert!(
        catalogue.diagnostics.iter().any(|d| d.contains("mismatch")
            || d.contains("checksum")
            || d.contains("parse failed")),
        "expected record/content diagnostic, got {:?}",
        catalogue.diagnostics
    );
}

#[test]
fn corrupt_or_traversal_bundle_ids_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("m.yang"),
        "module m { yang-version \"1\"; namespace \"urn:m\"; prefix m; revision \"2024-01-01\"; container c { leaf x { type string; } } }\n",
    )
    .unwrap();
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    let cat = compile_library(&library, true).unwrap();
    assert_eq!(cat.status, "complete");
    activate_compiled(&library, &cat.schema_id).unwrap();

    // Path traversal / invalid ids rejected.
    for bad in ["../escape", "ysc-../../x", "not-an-id", "ysc-zzzz"] {
        assert!(activate_compiled(&library, bad).is_err(), "accepted {bad}");
    }

    // Corrupt the active bundle → load fails closed.
    let bundle = dir
        .path()
        .join("lib")
        .join("compiled")
        .join(format!("{}.json", cat.schema_id));
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&bundle).unwrap()).unwrap();
    v["paths"] = serde_json::json!([]);
    std::fs::write(&bundle, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(
        load_active_catalogue(&library).is_err(),
        "corrupt bundle accepted"
    );
}

#[test]
fn failed_activation_preserves_previous_catalogue() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, leaf) in [("a", "x"), ("b", "y")] {
        std::fs::write(
            src.join(format!("{name}.yang")),
            format!("module {name} {{ yang-version \"1\"; namespace \"urn:{name}\"; prefix {name}; revision \"2024-01-01\"; container c {{ leaf {leaf} {{ type string; }} }} }}\n"),
        )
        .unwrap();
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    let c1 = compile_library(&library, true).unwrap();
    activate_compiled(&library, &c1.schema_id).unwrap();

    // Remove module b → different (still complete? no—fewer modules, still
    // complete) catalogue; activate fails only for bogus id.
    assert!(activate_compiled(&library, "ysc-00000000000000000000000000000000").is_err());
    let active = load_active_catalogue(&library).unwrap().unwrap();
    assert_eq!(
        active.schema_id, c1.schema_id,
        "failed activation disturbed active"
    );
}

#[test]
fn parent_if_feature_gates_descendants() {
    let yang = r#"module featgate {
  yang-version "1.1";
  namespace "urn:t:fg";
  prefix fg;
  revision "2024-01-01";
  feature gated;
  container parent {
    if-feature fg:gated;
    leaf child { type string; }
    list items {
      key name;
      leaf name { type string; }
      leaf inner { type uint32; }
    }
  }
  container open { leaf free { type string; } }
}
"#;
    let cat = compile_parsed(
        vec![inline_input("featgate", yang)],
        &CompileOptions::default(),
    );
    assert_eq!(cat.status, "complete", "diags={:?}", cat.diagnostics);
    for p in [
        "/parent",
        "/parent/child",
        "/parent/items[name]",
        "/parent/items[name]/inner",
    ] {
        let e = find_path(&cat, p).unwrap_or_else(|| panic!("missing {}", p));
        assert!(
            e.required_features.iter().any(|f| f.contains("gated")),
            "{} missing inherited gate: {:?}",
            p,
            e.required_features
        );
    }
    let open = find_path(&cat, "/open/free").unwrap();
    assert!(open.required_features.is_empty());
    // Boolean expression canonicalization: 'gated and gated' stays an expr,
    // not a bogus feature name.
    let yang2 = yang.replace(
        "if-feature fg:gated;",
        "if-feature \"fg:gated and fg:gated\";",
    );
    let cat2 = compile_parsed(
        vec![inline_input("featgate", &yang2)],
        &CompileOptions::default(),
    );
    let e = find_path(&cat2, "/parent/child").unwrap();
    assert!(
        e.required_features.iter().any(|f| f.contains(" and ")),
        "boolean expr not preserved: {:?}",
        e.required_features
    );
    // filter_by_features evaluates the expression.
    let enabled: std::collections::BTreeSet<String> =
        ["fg:gated".to_string()].into_iter().collect();
    assert_eq!(
        filter_by_features(&cat2.paths, &enabled).len(),
        cat2.paths.len()
    );
    let none: std::collections::BTreeSet<String> = Default::default();
    let gated_out = filter_by_features(&cat2.paths, &none);
    assert!(gated_out.iter().all(|e| e.path.starts_with("/open")));
}

#[test]
fn namespace_qualified_augment_targets_correct_module() {
    // Two modules expose identically named roots; a prefixed augment must
    // hit the module its prefix resolves to, not first local-name match.
    let mod_a = r#"module moda {
  yang-version "1.1";
  namespace "urn:t:a";
  prefix ma;
  revision "2024-01-01";
  container system { leaf a { type string; } }
}
"#;
    let mod_b = r#"module modb {
  yang-version "1.1";
  namespace "urn:t:b";
  prefix mb;
  revision "2024-01-01";
  container system { leaf b { type string; } }
}
"#;
    let aug = r#"module augm {
  yang-version "1.1";
  namespace "urn:t:aug";
  prefix ag;
  revision "2024-01-01";
  import moda { prefix ma; }
  import modb { prefix mb; }
  augment "/ma:system" { leaf only-a { type string; } }
  augment "/mb:system" { leaf only-b { type string; } }
}
"#;
    let cat = compile_parsed(
        vec![
            inline_input("moda", mod_a),
            inline_input("modb", mod_b),
            inline_input("augm", aug),
        ],
        &CompileOptions::default(),
    );
    assert_eq!(cat.status, "complete", "diags={:?}", cat.diagnostics);
    let a_sys = cat
        .paths
        .iter()
        .find(|p| p.module == "moda" && p.path == "/system")
        .unwrap();
    let _ = a_sys;
    // Augmented leaves carry augment provenance: they are declared by
    // `augm`, emitted under the target module's tree.
    assert!(
        cat.paths
            .iter()
            .any(|p| p.path == "/system/only-a" && p.module == "augm")
    );
    assert!(
        cat.paths
            .iter()
            .any(|p| p.path == "/system/only-b" && p.module == "augm")
    );
    assert!(
        !cat.paths
            .iter()
            .any(|p| p.path == "/system/only-a" && p.module == "moda" && p.module_prefix == "ma")
    );
}

#[test]
fn ambiguous_unprefixed_target_is_diagnosed() {
    let mod_a = r#"module moda2 {
  yang-version "1.1";
  namespace "urn:t:a2";
  prefix ma;
  revision "2024-01-01";
  container dup { leaf a { type string; } }
}
"#;
    let mod_b = r#"module modb2 {
  yang-version "1.1";
  namespace "urn:t:b2";
  prefix mb;
  revision "2024-01-01";
  container dup { leaf b { type string; } }
}
"#;
    let aug = r#"module augm2 {
  yang-version "1.1";
  namespace "urn:t:aug2";
  prefix ag;
  revision "2024-01-01";
  import moda2 { prefix ma; }
  import modb2 { prefix mb; }
  augment "/dup" { leaf x { type string; } }
}
"#;
    let cat = compile_parsed(
        vec![
            inline_input("moda2", mod_a),
            inline_input("modb2", mod_b),
            inline_input("augm2", aug),
        ],
        &CompileOptions::default(),
    );
    assert_eq!(cat.status, "partial");
    assert!(
        cat.diagnostics.iter().any(|d| d.contains("ambiguous")),
        "expected ambiguity diagnostic, got {:?}",
        cat.diagnostics
    );
}

#[test]
fn deviate_not_supported_prunes_node() {
    let base = r#"module dbase {
  yang-version "1.1";
  namespace "urn:t:db";
  prefix db;
  revision "2024-01-01";
  container c {
    leaf keep { type string; }
    leaf drop { type string; }
  }
}
"#;
    let dev = r#"module ddev {
  yang-version "1.1";
  namespace "urn:t:dd";
  prefix dd;
  revision "2024-01-01";
  import dbase { prefix db; }
  deviation "/db:c/drop" {
    deviate not-supported;
  }
}
"#;
    let cat = compile_parsed(
        vec![inline_input("dbase", base), inline_input("ddev", dev)],
        &CompileOptions::default(),
    );
    assert!(cat.paths.iter().any(|p| p.path == "/c/keep"));
    assert!(
        !cat.paths.iter().any(|p| p.path == "/c/drop"),
        "not-supported node must be pruned"
    );
}

#[test]
fn parser_rejects_unterminated_module_and_accepts_unquoted_keys() {
    // Missing closing brace → hard parse error.
    let truncated = "module trunc { yang-version \"1\"; namespace \"urn:t\"; prefix t;";
    assert!(bonsai::yang_schema::parse_module(truncated).is_err());

    // Unquoted multi-key `key a b;` parses as two keys.
    let yang = r#"module keys {
  yang-version "1.1";
  namespace "urn:t:k";
  prefix k;
  revision "2024-01-01";
  list pair {
    key a b;
    leaf a { type string; }
    leaf b { type string; }
  }
}
"#;
    let cat = compile_parsed(vec![inline_input("keys", yang)], &CompileOptions::default());
    let e = find_path(&cat, "/pair[a][b]").unwrap_or_else(|| {
        panic!(
            "status={} diags={:?} paths={:?}",
            cat.status,
            cat.diagnostics,
            cat.paths.iter().map(|p| p.path.clone()).collect::<Vec<_>>()
        )
    });
    assert_eq!(e.keys, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn slash_bearing_augment_target_parses() {
    // Absolute path target beginning with '/'.
    let base = r#"module sbase {
  yang-version "1.1";
  namespace "urn:t:sb";
  prefix sb;
  revision "2024-01-01";
  container c { leaf x { type string; } }
}
"#;
    let aug = r#"module saug {
  yang-version "1.1";
  namespace "urn:t:sa";
  prefix sa;
  revision "2024-01-01";
  import sbase { prefix sb; }
  augment "/sb:c" { leaf added { type string; } }
}
"#;
    let cat = compile_parsed(
        vec![inline_input("sbase", base), inline_input("saug", aug)],
        &CompileOptions::default(),
    );
    assert!(
        cat.paths.iter().any(|p| p.path == "/c/added"),
        "diags={:?}",
        cat.diagnostics
    );
}

#[test]
fn unsupported_refine_semantics_are_diagnosed() {
    let base = r#"module rbase {
  yang-version "1.1";
  namespace "urn:t:rb";
  prefix rb;
  revision "2024-01-01";
  grouping g {
    leaf f { type string; mandatory true; }
  }
  container c { uses g; }
}
"#;
    let refined = r#"module rref {
  yang-version "1.1";
  namespace "urn:t:rr";
  prefix rr;
  revision "2024-01-01";
  import rbase { prefix rb; }
  container top {
    uses rb:g {
      refine f {
        mandatory false;
        must "custom-must";
      }
    }
  }
}
"#;
    let cat = compile_parsed(
        vec![inline_input("rbase", base), inline_input("rref", refined)],
        &CompileOptions::default(),
    );
    assert_eq!(cat.status, "partial", "diags={:?}", cat.diagnostics);
    assert!(
        cat.diagnostics
            .iter()
            .any(|d| d.contains("refine") && d.contains("must")),
        "expected unsupported-refine diagnostic, got {:?}",
        cat.diagnostics
    );
    // Supported refine still applied: mandatory false on /top/f.
    let e = find_path(&cat, "/top/f").unwrap();
    assert!(!e.mandatory);
}

#[test]
fn deep_nesting_is_bounded() {
    // Pathological nesting must error, not overflow the stack.
    let mut yang = String::from(
        "module deep { yang-version \"1\"; namespace \"urn:t:d\"; prefix d; revision \"2024-01-01\";\n",
    );
    for i in 0..500 {
        yang.push_str(&format!("container n{} {{\n", i));
    }
    yang.push_str("leaf x { type string; }\n");
    for _ in 0..500 {
        yang.push_str("}\n");
    }
    yang.push_str("}\n");
    // Either parse fails or compile reports depth diagnostics — never panic.
    match bonsai::yang_schema::parse_module(&yang) {
        Ok(m) => {
            let cat = compile_parsed(
                vec![ModuleInput {
                    module: m,
                    vendor_scope: "t".into(),
                    source_kind: "t".into(),
                    source_ref: "deep".into(),
                    trust: "trusted".into(),
                    checksum_sha256: String::new(),
                    approved: false,
                    license: String::new(),
                    platforms: vec![],
                    pinned_ref: String::new(),
                    source_id: String::new(),
                }],
                &CompileOptions::default(),
            );
            assert_ne!(cat.status, "complete");
        }
        Err(_) => {}
    }
}

#[test]
fn conflict_promotion_requires_explicit_approval() {
    // Equal trust, different content, later import: incumbent (lowest
    // checksum) must NOT be silently displaced.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let opts = |refname: &str| YangImportOptions {
        source_kind: "test".into(),
        source_ref: refname.into(),
        vendor_scope: "v".into(),
        trust: "trusted".into(),
        license: String::new(),
        platforms: vec![],
        pinned_ref: String::new(),
        source_id: String::new(),
        filename_prefixes: vec![],
    };
    let src1 = dir.path().join("s1");
    let src2 = dir.path().join("s2");
    std::fs::create_dir_all(&src1).unwrap();
    std::fs::create_dir_all(&src2).unwrap();
    std::fs::write(
        src1.join("dup.yang"),
        "module dup { yang-version \"1\"; namespace \"urn:d1\"; prefix d; revision \"2024-01-01\"; container a { leaf x { type string; } } }\n",
    )
    .unwrap();
    std::fs::write(
        src2.join("dup.yang"),
        "module dup { yang-version \"1\"; namespace \"urn:d2\"; prefix d; revision \"2024-01-01\"; container b { leaf y { type uint32; } } }\n",
    )
    .unwrap();
    library
        .import_directory(&src1, &opts("one"), Path::new("config/path_profiles"))
        .unwrap();
    // Ensure distinct import timestamps so incumbent ordering is
    // unambiguous (nanosecond clock makes ties vanishingly rare, but the
    // selection policy must not depend on clock resolution).
    std::thread::sleep(std::time::Duration::from_millis(1));
    library
        .import_directory(&src2, &opts("two"), Path::new("config/path_profiles"))
        .unwrap();
    let state = library.load_state().unwrap();
    let dups: Vec<_> = state
        .modules
        .iter()
        .filter(|m| m.module_name == "dup")
        .collect();
    assert_eq!(dups.len(), 2);
    // Winner is deterministic: the INCUMBENT (earliest import) is stable.
    // A later equal-trust import can never silently displace it,
    // regardless of which checksum sorts lower.
    let incumbent = dups.iter().min_by_key(|m| m.imported_at_ns).unwrap();
    let active = library.active_modules().unwrap();
    let winner = active.iter().find(|m| m.module_name == "dup").unwrap();
    assert_eq!(winner.checksum_sha256, incumbent.checksum_sha256);
    // Explicit approval promotes the OTHER record.
    let other = dups
        .iter()
        .find(|m| m.checksum_sha256 != winner.checksum_sha256)
        .unwrap();
    library
        .approve_module("dup", &other.revision, &other.checksum_sha256)
        .unwrap();
    let active2 = library.active_modules().unwrap();
    let winner2 = active2.iter().find(|m| m.module_name == "dup").unwrap();
    assert_eq!(winner2.checksum_sha256, other.checksum_sha256);
    assert!(winner2.approved);
}

#[test]
fn compiled_index_refresh_is_memory_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("m.yang"),
        "module m {\n yang-version \"1\";\n namespace \"urn:m\";\n prefix m;\n revision \"2024-01-01\";\n container c { leaf l { type string; } }\n}\n",
    )
    .unwrap();
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    compile_library(&library, true).unwrap();

    // A large foreign bundle: valid JSON with header fields early and
    // ~64 MiB of uninteresting payload. Index refresh must stream it —
    // RSS must not grow proportionally to the file size.
    let compiled = library.root().join("compiled");
    let big_id = format!("ysc-{}", "a".repeat(32));
    let pad = "x".repeat(64 * 1024 * 1024);
    let raw = format!(
        "{{\"schema_version\":1,\"schema_id\":\"{big_id}\",\"compiled_at_ns\":7,\"module_count\":3,\"path_count\":11,\"status\":\"complete\",\"modules\":[],\"paths\":[],\"diagnostics\":[],\"pad\":\"{pad}\"}}"
    );
    std::fs::write(compiled.join(format!("{big_id}.json")), raw).unwrap();

    fn rss_kb() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .unwrap_or(0)
    }
    let before = rss_kb();
    let infos = list_compiled(&library).unwrap();
    let after = rss_kb();
    let info = infos
        .iter()
        .find(|i| i.schema_id == big_id)
        .expect("large bundle indexed");
    assert_eq!(info.module_count, 3);
    assert_eq!(info.path_count, 11);
    assert_eq!(info.compiled_at_ns, 7);
    let delta_kb = after.saturating_sub(before);
    assert!(
        delta_kb < 32 * 1024,
        "index refresh retained ~{} KiB for a 64 MiB bundle",
        delta_kb
    );
}

#[test]
fn concurrent_persist_and_activate_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("m.yang"),
        "module m {\n yang-version \"1\";\n namespace \"urn:m\";\n prefix m;\n revision \"2024-01-01\";\n container c { leaf l { type string; } }\n}\n",
    )
    .unwrap();
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
                source_id: String::new(),
                filename_prefixes: vec![],
            },
            Path::new("config/path_profiles"),
        )
        .unwrap();
    let cat = compile_library(&library, true).unwrap();

    // Concurrent persists of the same identity, concurrent activations and
    // concurrent readers must never publish a torn bundle or pointer.
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| persist_compiled(&library, &cat).unwrap());
        }
        for _ in 0..2 {
            s.spawn(|| activate_compiled(&library, &cat.schema_id).unwrap());
        }
        for _ in 0..2 {
            s.spawn(|| {
                let _ = load_active_catalogue(&library);
                let _ = list_compiled(&library);
            });
        }
    });
    let active = load_active_catalogue(&library).unwrap().unwrap();
    assert_eq!(active.schema_id, cat.schema_id);
    // The published bundle must still verify.
    let bundles = list_compiled(&library).unwrap();
    assert!(bundles.iter().any(|b| b.schema_id == cat.schema_id));
}

fn tiny_yang(ns: &str) -> String {
    format!(
        "module m {{ yang-version \"1\"; namespace \"{ns}\"; prefix m; revision \"2024-01-01\"; container c {{ leaf l {{ type string; }} }} }}\n"
    )
}

#[test]
fn first_publication_race_is_atomic_no_replace() {
    // Two catalogues with IDENTICAL schema_id but different bytes
    // (compiled_at_ns is excluded from the identity hash). Concurrent
    // first-publication attempts on an ABSENT destination must be
    // first-writer-wins: the loser verifies the winner and never
    // overwrites already-published bytes.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cat_a = compile_parsed(
        vec![inline_input("race", &tiny_yang("urn:race"))],
        &CompileOptions::default(),
    );
    let mut cat_b = cat_a.clone();
    cat_b.compiled_at_ns += 1;
    assert_eq!(cat_a.schema_id, cat_b.schema_id);
    let bytes_a = serde_json::to_vec(&cat_a).unwrap();
    let bytes_b = serde_json::to_vec(&cat_b).unwrap();
    assert_ne!(bytes_a, bytes_b, "test requires distinct published bytes");

    // Competing first-publication attempts from many threads on an absent
    // destination — exercises the no-replace link path, not the
    // already-persisted fast path.
    std::thread::scope(|s| {
        for i in 0..8 {
            let cat = if i % 2 == 0 { &cat_a } else { &cat_b };
            s.spawn(|| persist_compiled(&library, cat).unwrap());
        }
    });

    let cdir = dir.path().join("lib").join("compiled");
    let bundle = cdir.join(format!("{}.json", cat_a.schema_id));
    let published = std::fs::read(&bundle).unwrap();
    assert!(
        published == bytes_a || published == bytes_b,
        "published bytes must be one intact contender, never a torn mix"
    );
    // No stray tmp files left behind by losing writers.
    let tmps: Vec<_> = std::fs::read_dir(&cdir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with('.') && n.ends_with(".tmp"))
        })
        .collect();
    assert!(tmps.is_empty(), "losing writers must clean up tmp files");

    // Re-persisting the LOSER is an idempotent verified no-op: first
    // writer wins and published bytes never change.
    let loser = if published == bytes_a { &cat_b } else { &cat_a };
    persist_compiled(&library, loser).unwrap();
    assert_eq!(std::fs::read(&bundle).unwrap(), published);
    // And the bundle still verifies through the normal read path.
    let bundles = list_compiled(&library).unwrap();
    assert!(bundles.iter().any(|b| b.schema_id == cat_a.schema_id));
}

#[test]
fn trust_update_fails_closed_on_conflicting_records() {
    // set_module_trust must not silently pick one of several same
    // name+revision records — an ambiguous trust bump could otherwise
    // promote a non-selected variant around the approval invariant.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let opts = |refname: &str| YangImportOptions {
        source_kind: "test".into(),
        source_ref: refname.into(),
        vendor_scope: "v".into(),
        trust: "trusted".into(),
        license: String::new(),
        platforms: vec![],
        pinned_ref: String::new(),
        source_id: String::new(),
        filename_prefixes: vec![],
    };
    for (name, body) in [
        ("s1", "container a { leaf x { type string; } }"),
        ("s2", "container b { leaf y { type uint32; } }"),
    ] {
        let src = dir.path().join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("dup.yang"),
            format!(
                "module dup {{ yang-version \"1\"; namespace \"urn:{}\"; prefix d; revision \"2024-01-01\"; {} }}\n",
                name, body
            ),
        )
        .unwrap();
        library
            .import_directory(&src, &opts(name), Path::new("config/path_profiles"))
            .unwrap();
    }
    let err = library
        .set_module_trust("dup", None, "experimental")
        .unwrap_err();
    assert!(
        err.to_string().contains("ambiguous"),
        "expected ambiguity error, got: {err}"
    );
    // Trust state is unchanged — neither conflicting record was mutated.
    let state = library.load_state().unwrap();
    assert!(
        state
            .modules
            .iter()
            .filter(|m| m.module_name == "dup")
            .all(|m| m.trust == "trusted")
    );
}

#[test]
fn unverified_bundle_metadata_cannot_shape_index() {
    // A foreign/content-swapped bundle (embedded schema_id != filename)
    // must not contribute retention/index metadata — its claimed
    // timestamp must never let it outrank or evict verified bundles.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cat_a = compile_parsed(
        vec![inline_input("idxa", &tiny_yang("urn:idxa"))],
        &CompileOptions::default(),
    );
    persist_compiled(&library, &cat_a).unwrap();

    let cdir = dir.path().join("lib").join("compiled");
    let forged_name = format!("ysc-{}", "0".repeat(32));
    let forged_embedded = format!("ysc-{}", "f".repeat(32));
    std::fs::write(
        cdir.join(format!("{forged_name}.json")),
        format!(
            "{{\"schema_id\":\"{forged_embedded}\",\"compiled_at_ns\":{},\"module_count\":999,\"path_count\":999,\"diagnostics\":[]}}",
            i64::MAX
        ),
    )
    .unwrap();

    // A second persist triggers index refresh + retention pass.
    let cat_b = compile_parsed(
        vec![inline_input("idxb", &tiny_yang("urn:idxb"))],
        &CompileOptions::default(),
    );
    persist_compiled(&library, &cat_b).unwrap();

    let idx_raw = std::fs::read_to_string(cdir.join("index.json")).unwrap();
    let idx: Vec<serde_json::Value> = serde_json::from_str(&idx_raw).unwrap();
    assert!(
        idx.iter().all(|e| e["schema_id"] != forged_name),
        "unverified bundle must not appear in the compiled index"
    );
    assert!(idx.iter().any(|e| e["schema_id"] == cat_a.schema_id));
    assert!(idx.iter().any(|e| e["schema_id"] == cat_b.schema_id));
    // Forged file remains on disk (no silent deletion) but is invisible
    // to index-driven consumers and sorts as epoch-0 for retention.
    assert!(cdir.join(format!("{forged_name}.json")).exists());
}

#[test]
fn forged_bundle_metadata_cannot_evict_verified_bundles() {
    // A forged bundle can carry a valid `ysc-<32 hex>` filename, an
    // embedded schema_id matching that filename, syntactically valid
    // catalogue JSON whose semantic content does NOT hash to that
    // identity, and a fabricated compiled_at_ns of i64::MAX. Header
    // checks alone cannot detect this; retention must verify semantic
    // integrity before trusting the timestamp, or the forged bundle
    // occupies a retention slot and evicts a legitimate bundle.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();

    // 40 legitimate bundles with strictly increasing compiled_at_ns —
    // retention keeps the newest 32 (L8..=L39) and evicts L0..=L7.
    let mut legit_ids = Vec::new();
    for i in 0..40u64 {
        let mut cat = compile_parsed(
            vec![inline_input(
                &format!("legit{i}"),
                &tiny_yang(&format!("urn:legit{i}")),
            )],
            &CompileOptions::default(),
        );
        cat.compiled_at_ns = 1_000 + i as i64;
        persist_compiled(&library, &cat).unwrap();
        legit_ids.push(cat.schema_id.clone());
    }
    let cdir = dir.path().join("lib").join("compiled");
    // Sanity: steady-state retention already evicted the oldest legit
    // bundles before the forged file ever appeared.
    assert!(cdir.join(format!("{}.json", legit_ids[7])).exists() == false);
    assert!(cdir.join(format!("{}.json", legit_ids[8])).exists());

    // Forge: valid filename + matching embedded id + invalid semantic
    // content + maximum timestamp.
    let forged_id = format!("ysc-{}", "b".repeat(32));
    std::fs::write(
        cdir.join(format!("{forged_id}.json")),
        format!(
            "{{\"schema_version\":1,\"schema_id\":\"{forged_id}\",\"compiled_at_ns\":{},\"module_count\":0,\"path_count\":0,\"modules\":[],\"paths\":[],\"diagnostics\":[]}}",
            i64::MAX
        ),
    )
    .unwrap();

    // Trigger a retention pass.
    list_compiled(&library).unwrap();

    // The forged bundle must not survive retention: it is proven invalid
    // and evicted rather than trusted.
    assert!(
        !cdir.join(format!("{forged_id}.json")).exists(),
        "forged bundle must be evicted by verified retention"
    );
    // Every legitimate retained bundle must still be present — the
    // forged i64::MAX timestamp must not have displaced L8 (the oldest
    // bundle that retention is required to keep).
    for id in &legit_ids[8..] {
        assert!(
            cdir.join(format!("{id}.json")).exists(),
            "legitimate retained bundle {id} was evicted by forged metadata"
        );
    }
    // And the retained count stays within the bound.
    let remaining: Vec<_> = std::fs::read_dir(&cdir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some("json")
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| n != "index.json" && n != "active.json" && !n.starts_with('.'))
        })
        .collect();
    assert_eq!(remaining.len(), 32);
}

#[test]
fn unreadable_bundle_beyond_retention_cutoff_is_preserved() {
    // Retention must classify EVERY eviction candidate, including files
    // sorted beyond the cutoff: verified and proven-invalid bundles may be
    // evicted, but a bundle whose integrity cannot be established (here a
    // directory where a bundle file is expected — stat succeeds, read
    // fails) must be preserved even though that temporarily exceeds
    // MAX_COMPILED_BUNDLES.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cdir = dir.path().join("lib").join("compiled");

    // MAX_COMPILED_BUNDLES legitimate bundles fill every retention slot.
    for i in 0..bonsai::yang_schema::MAX_COMPILED_BUNDLES {
        let cat = compile_parsed(
            vec![inline_input(
                &format!("keep{i}"),
                &tiny_yang(&format!("urn:keep{i}")),
            )],
            &CompileOptions::default(),
        );
        persist_compiled(&library, &cat).unwrap();
    }

    // An unreadable "bundle": a directory named like a compiled bundle.
    // verify_bundle cannot establish integrity -> VerifyFailure::Unreadable.
    let unreadable_id = format!("ysc-{}", "d".repeat(32));
    std::fs::create_dir(cdir.join(format!("{unreadable_id}.json"))).unwrap();

    // A proven-invalid bundle sorting alongside it (unparseable header ->
    // epoch 0, beyond the cutoff) must still be evicted.
    let invalid_id = format!("ysc-{}", "e".repeat(32));
    std::fs::write(cdir.join(format!("{invalid_id}.json")), b"not json").unwrap();

    // Trigger the retention pass.
    list_compiled(&library).unwrap();

    assert!(
        cdir.join(format!("{unreadable_id}.json")).exists(),
        "unreadable bundle beyond the retention cutoff must be preserved"
    );
    assert!(
        !cdir.join(format!("{invalid_id}.json")).exists(),
        "proven-invalid bundle beyond the cutoff must be evicted"
    );
    // 32 verified bundles + 1 preserved unreadable file: the bound is
    // temporarily exceeded, explicitly and only for unverifiable data.
    let remaining: Vec<_> = std::fs::read_dir(&cdir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some("json")
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| n != "index.json" && n != "active.json" && !n.starts_with('.'))
        })
        .collect();
    assert_eq!(
        remaining.len(),
        bonsai::yang_schema::MAX_COMPILED_BUNDLES + 1
    );
}

#[test]
fn corrupt_active_pointer_fails_closed() {
    // An unreadable or malformed active.json must surface as an error, not
    // be swallowed into `None` — destructive retention cannot prove which
    // bundle is active without it.
    let dir = tempfile::tempdir().unwrap();
    let library = YangLibrary::open(dir.path().join("lib"), dir.path().join("cache"), "K").unwrap();
    let cat = compile_parsed(
        vec![inline_input("ptr", &tiny_yang("urn:ptr"))],
        &CompileOptions::default(),
    );
    persist_compiled(&library, &cat).unwrap();
    let cdir = dir.path().join("lib").join("compiled");
    let bundle_file = cdir.join(format!("{}.json", cat.schema_id));

    // Case 1: unparseable pointer JSON.
    std::fs::write(cdir.join("active.json"), b"{not json").unwrap();
    assert!(
        list_compiled(&library).is_err(),
        "malformed active pointer must fail closed"
    );
    assert!(bundle_file.exists(), "no bundle may be deleted");

    // Case 2: parseable pointer naming a non-bundle id — would silently
    // unprotect every real bundle if treated as authoritative.
    std::fs::write(
        cdir.join("active.json"),
        br#"{"schema_id":"not-a-schema-id"}"#,
    )
    .unwrap();
    assert!(
        list_compiled(&library).is_err(),
        "active pointer with invalid schema id must fail closed"
    );
    assert!(bundle_file.exists(), "no bundle may be deleted");
}

#[test]
fn nokia_dialect_fixture_parses_and_compiles() {
    // M6F-B: Nokia SR Linux YANG dialect — unquoted revision dates,
    // prefixed extension instances, string/bare `presence`, bare and
    // '/'-bearing enum names, typedefs with concatenated patterns and
    // error-message sub-blocks. Live evidence: 11-module SR Linux
    // closure compiles to 419 paths with zero diagnostics.
    let cat = compile_parsed(vec![module_input("test-nokia")], &CompileOptions::default());
    assert_eq!(
        cat.status,
        "complete",
        "diags={:?} modules={:?}",
        cat.diagnostics,
        cat.modules
            .iter()
            .map(|m| (m.name.clone(), m.node_count, m.resolution.clone()))
            .collect::<Vec<_>>()
    );
    for expected in [
        "/system/hostname",
        "/system/interface[name]/statistics/in-packets",
        "/system/interface[name]/statistics/state",
        "/system/checkpoint",
    ] {
        assert!(
            cat.paths.iter().any(|p| p.path == expected),
            "missing {expected}; paths={:?}",
            cat.paths.iter().map(|p| p.path.clone()).collect::<Vec<_>>()
        );
    }
}

// ---------------------------------------------------------------------------
// M6F-F PR1 — explicit module-revision selection
// ---------------------------------------------------------------------------

fn rev_module(name: &str, revision: &str, extra_leaf: &str) -> String {
    format!(
        r#"module {name} {{
  yang-version "1.1";
  namespace "urn:example:{name}";
  prefix "{name}";
  organization "Example Org";
  revision "{revision}" {{
    description "rev";
  }}
  container state {{
    leaf marker {{
      type string;
    }}
    {extra_leaf}
  }}
}}"#
    )
}

fn pinned_input(name: &str, yang: &str, tag: &str) -> ModuleInput {
    let module = bonsai::yang_schema::parse_module(yang).expect("pin fixture parses");
    ModuleInput {
        module,
        vendor_scope: "test".to_string(),
        source_kind: "inline".to_string(),
        source_ref: format!("{name}@{tag}"),
        trust: "trusted".to_string(),
        checksum_sha256: format!("pin-{name}-{tag}"),
        approved: false,
        license: String::new(),
        platforms: Vec::new(),
        pinned_ref: String::new(),
        source_id: String::new(),
    }
}

#[test]
fn revision_pin_selects_exact_older_revision() {
    let old = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2020-01-01", ""),
        "2020-01-01",
    );
    let new = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2024-01-01", "leaf added-2024 { type uint32; }"),
        "2024-01-01",
    );
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-mod".to_string(), "2020-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins.clone(),
        ..CompileOptions::default()
    };
    let cat = compile_parsed(vec![new, old], &opts);
    assert_eq!(cat.modules.len(), 1);
    assert_eq!(cat.modules[0].name, "pin-mod");
    assert_eq!(cat.modules[0].revision, "2020-01-01");
    assert_eq!(cat.module_selection, pins);
    // The 2024-only leaf must not appear in the pinned catalogue.
    assert!(
        !cat.paths.iter().any(|p| p.path.contains("added-2024")),
        "pinned catalogue leaked newer-revision content: {:?}",
        cat.paths.iter().map(|p| p.path.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn revision_pin_absent_keeps_default_newest_policy() {
    // Compatibility: no pins => identical behavior to before M6F-F.
    let old = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2020-01-01", ""),
        "2020-01-01",
    );
    let new = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2024-01-01", ""),
        "2024-01-01",
    );
    let cat = compile_parsed(vec![old, new], &CompileOptions::default());
    assert_eq!(cat.modules[0].revision, "2024-01-01");
    assert!(cat.module_selection.is_empty());
}

#[test]
fn revision_pin_missing_revision_fails_closed() {
    let only = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2024-01-01", ""),
        "2024-01-01",
    );
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-mod".to_string(), "1999-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let cat = compile_parsed(vec![only], &opts);
    assert!(
        cat.diagnostics
            .iter()
            .any(|d| d.contains("pinned to revision '1999-01-01'")),
        "expected pin diagnostic, got {:?}",
        cat.diagnostics
    );
    assert!(
        cat.modules.is_empty(),
        "unpinned revision must not substitute"
    );
    assert_ne!(cat.status, "complete");
}

#[test]
fn revision_pin_absent_module_fails_closed() {
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("ghost-mod".to_string(), "2020-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let cat = compile_parsed(
        vec![pinned_input(
            "real-mod",
            &rev_module("real-mod", "2024-01-01", ""),
            "2024-01-01",
        )],
        &opts,
    );
    assert!(
        cat.diagnostics
            .iter()
            .any(|d| d.contains("'ghost-mod' pinned to revision '2020-01-01'")),
        "got {:?}",
        cat.diagnostics
    );
    // The unrelated module still compiles; the pin itself is diagnostic.
    assert_eq!(cat.modules.len(), 1);
    assert_ne!(cat.status, "complete");
}

#[test]
fn revision_pin_enforces_import_revision_match() {
    let child_yang = r#"module pin-child {
  yang-version "1.1";
  namespace "urn:example:pin-child";
  prefix "pc";
  import pin-base {
    prefix "pb";
    revision "2020-01-01";
  }
  container state {
    leaf marker { type string; }
  }
}"#;
    let mk_inputs = || {
        vec![
            pinned_input(
                "pin-base",
                &rev_module("pin-base", "2024-01-01", ""),
                "2024-01-01",
            ),
            pinned_input(
                "pin-base",
                &rev_module("pin-base", "2020-01-01", ""),
                "2020-01-01",
            ),
            pinned_input("pin-child", child_yang, "child"),
        ]
    };

    // Pinning the import-declared revision resolves cleanly.
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-base".to_string(), "2020-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let cat = compile_parsed(mk_inputs(), &opts);
    assert!(
        !cat.diagnostics
            .iter()
            .any(|d| d.contains("imports 'pin-base'")),
        "compatible pin must not produce import diagnostics: {:?}",
        cat.diagnostics
    );
    assert_eq!(cat.modules.len(), 2);

    // Pinning a different revision makes the declared import revision
    // mismatch explicit — never silently satisfied.
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-base".to_string(), "2024-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let cat = compile_parsed(mk_inputs(), &opts);
    assert!(
        cat.diagnostics.iter().any(|d| d.contains(
            "imports 'pin-base'@'2020-01-01' but compiled set provides revision '2024-01-01'"
        )),
        "expected import revision mismatch, got {:?}",
        cat.diagnostics
    );
    assert_ne!(cat.status, "complete");
}

#[test]
fn revision_pin_is_deterministic_across_input_order() {
    let mk = || {
        vec![
            pinned_input(
                "pin-mod",
                &rev_module("pin-mod", "2020-01-01", ""),
                "2020-01-01",
            ),
            pinned_input(
                "pin-mod",
                &rev_module("pin-mod", "2024-01-01", ""),
                "2024-01-01",
            ),
            pinned_input(
                "pin-other",
                &rev_module("pin-other", "2019-05-05", ""),
                "2019-05-05",
            ),
        ]
    };
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-mod".to_string(), "2020-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let a = compile_parsed(mk(), &opts);
    let mut reversed = mk();
    reversed.reverse();
    let b = compile_parsed(reversed, &opts);
    assert_eq!(a.schema_id, b.schema_id);
    assert_eq!(a.diagnostics, b.diagnostics);
}

#[test]
fn revision_pin_matching_default_dedups_schema_id() {
    // A pin that selects exactly what the default policy would keep, on a
    // single-revision module, adds no diagnostics — the compiled content
    // is identical and must share the same content-addressed schema_id.
    let mk = || {
        pinned_input(
            "pin-mod",
            &rev_module("pin-mod", "2024-01-01", ""),
            "2024-01-01",
        )
    };
    let unpinned = compile_parsed(vec![mk()], &CompileOptions::default());
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-mod".to_string(), "2024-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        ..CompileOptions::default()
    };
    let pinned = compile_parsed(vec![mk()], &opts);
    assert_eq!(unpinned.schema_id, pinned.schema_id);
    assert_eq!(unpinned.status, pinned.status);
}

#[test]
fn revision_pin_rejects_untrusted_module() {
    // trusted_only compile: an untrusted pinned module is absent, so the
    // pin is unresolvable — fail closed, never compile untrusted content.
    let mut untrusted = pinned_input(
        "pin-mod",
        &rev_module("pin-mod", "2020-01-01", ""),
        "2020-01-01",
    );
    untrusted.trust = "experimental".to_string();
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("pin-mod".to_string(), "2020-01-01".to_string());
    let opts = CompileOptions {
        module_revisions: pins,
        trusted_only: true,
    };
    let cat = compile_parsed(vec![untrusted], &opts);
    assert!(cat.modules.is_empty());
    assert!(
        cat.diagnostics
            .iter()
            .any(|d| d.contains("pinned to revision '2020-01-01'")),
        "got {:?}",
        cat.diagnostics
    );
}

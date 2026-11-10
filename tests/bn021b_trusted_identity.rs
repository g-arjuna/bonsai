//! BN-021b: trusted exporter identity — canonical device IDs,
//! inventory-backed bindings, deterministic resolution, persistence.
//!
//! All data is synthetic; no credential material.

use bonsai::sqlite_store::SqliteStore;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, IdentityResolver, IdentityStatus,
    TelemetrySourceIdentity, TrustedBinding,
};

fn open_store() -> (tempfile::TempDir, SqliteStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SqliteStore::open(dir.path()).expect("open store");
    (dir, store)
}

fn device(address: &str, site: Option<&str>) -> CanonicalDevice {
    CanonicalDevice {
        device_id: String::new(),
        address: address.to_string(),
        site: site.map(str::to_string),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    }
}

fn binding(scope: &str, protocol: &str, exporter_id: &str, device_id: &str) -> TrustedBinding {
    TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: scope.to_string(),
        protocol: protocol.to_string(),
        exporter_id: exporter_id.to_string(),
        device_id: device_id.to_string(),
        match_transport: None,
        match_reported_identity: None,
        match_exporter_claim: None,
        match_observation_domain: None,
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    }
}

fn obs(transport: &str, protocol: &str, scope: &str) -> TelemetrySourceIdentity {
    TelemetrySourceIdentity::observed(transport)
        .with_protocol(protocol)
        .with_scope(scope)
        .with_exporter(format!("{protocol}:{transport}"))
}

fn active_bindings(store: &SqliteStore) -> Vec<TrustedBinding> {
    store
        .list_exporter_bindings(None, None, false)
        .expect("list bindings")
}

// ── A. Stable identity across management-IP changes ─────────────────────────

#[test]
fn stable_device_id_across_address_change() {
    let (_dir, store) = open_store();
    let dev = store
        .upsert_canonical_device(&device("10.0.0.1", Some("site-a")), "test")
        .unwrap();
    let device_id = dev.device_id.clone();

    let mut b = binding("site-a", "bmp", "exp-router-a", &device_id);
    b.match_exporter_claim = Some("bmp:frr1".to_string());
    store.upsert_exporter_binding(&b, "test").unwrap();

    // Observed at the original address.
    let o1 = obs("10.0.0.1", "bmp", "site-a").with_exporter("bmp:frr1");
    let r1 = IdentityResolver::resolve(&o1, &active_bindings(&store));
    assert_eq!(r1.status, IdentityStatus::Resolved);
    assert_eq!(r1.device_id.as_deref(), Some(device_id.as_str()));
    assert!(r1.may_update_authoritative_state());

    // Management address changes; same canonical device, no duplicate row.
    let mut moved = device("10.0.0.2", Some("site-a"));
    moved.device_id = device_id.clone();
    let updated = store.upsert_canonical_device(&moved, "test").unwrap();
    assert_eq!(updated.device_id, device_id);
    assert_eq!(updated.address, "10.0.0.2");
    assert_eq!(store.list_canonical_devices().unwrap().len(), 1);

    // Same exporter claim at the new transport address still resolves
    // to the same canonical device — the binding does not pin transport.
    let o2 = obs("10.0.0.2", "bmp", "site-a").with_exporter("bmp:frr1");
    let r2 = IdentityResolver::resolve(&o2, &active_bindings(&store));
    assert_eq!(r2.status, IdentityStatus::Resolved);
    assert_eq!(r2.device_id.as_deref(), Some(device_id.as_str()));
    // Transport provenance preserved.
    assert_eq!(r2.observed.transport_source, "10.0.0.2");
}

// ── B. Overlapping site addresses ────────────────────────────────────────────

#[test]
fn overlapping_addresses_do_not_cross_resolve() {
    let (_dir, store) = open_store();
    let dev_a = store
        .upsert_canonical_device(&device("10.0.0.5", Some("site-a")), "test")
        .unwrap();
    let dev_b = store
        .upsert_canonical_device(&device("10.0.0.5", Some("site-b")), "test")
        .unwrap();

    let mut ba = binding("site-a", "bmp", "exp-a", &dev_a.device_id);
    ba.match_transport = Some("10.0.0.5".to_string());
    store.upsert_exporter_binding(&ba, "test").unwrap();
    let mut bb = binding("site-b", "bmp", "exp-b", &dev_b.device_id);
    bb.match_transport = Some("10.0.0.5".to_string());
    store.upsert_exporter_binding(&bb, "test").unwrap();

    let ra = IdentityResolver::resolve(&obs("10.0.0.5", "bmp", "site-a"), &active_bindings(&store));
    assert_eq!(ra.status, IdentityStatus::Resolved);
    assert_eq!(ra.device_id.as_deref(), Some(dev_a.device_id.as_str()));

    let rb = IdentityResolver::resolve(&obs("10.0.0.5", "bmp", "site-b"), &active_bindings(&store));
    assert_eq!(rb.status, IdentityStatus::Resolved);
    assert_eq!(rb.device_id.as_deref(), Some(dev_b.device_id.as_str()));
    assert_ne!(ra.device_id, rb.device_id);

    // Unscoped observation cannot reach into either site scope.
    let unscoped = TelemetrySourceIdentity::observed("10.0.0.5")
        .with_protocol("bmp")
        .with_exporter("bmp:10.0.0.5");
    let ru = IdentityResolver::resolve(&unscoped, &active_bindings(&store));
    assert_eq!(ru.status, IdentityStatus::Provisional);
    assert!(ru.device_id.is_none());
}

// ── C. Two exporters behind NAT ──────────────────────────────────────────────

#[test]
fn nat_exporters_resolve_by_binding_not_source_ip() {
    let (_dir, store) = open_store();
    let dev1 = store
        .upsert_canonical_device(&device("192.0.2.1", Some("dc1")), "test")
        .unwrap();
    let dev2 = store
        .upsert_canonical_device(&device("192.0.2.2", Some("dc1")), "test")
        .unwrap();

    // Same transport IP (NAT), distinguished by reported sysName + claim.
    let mut b1 = binding("dc1", "bmp", "exp-rtr-1", &dev1.device_id);
    b1.match_transport = Some("203.0.113.9".to_string());
    b1.match_reported_identity = Some("frr1".to_string());
    store.upsert_exporter_binding(&b1, "test").unwrap();
    let mut b2 = binding("dc1", "bmp", "exp-rtr-2", &dev2.device_id);
    b2.match_transport = Some("203.0.113.9".to_string());
    b2.match_reported_identity = Some("frr2".to_string());
    store.upsert_exporter_binding(&b2, "test").unwrap();

    let r1 = IdentityResolver::resolve(
        &obs("203.0.113.9", "bmp", "dc1")
            .with_exporter("bmp:frr1")
            .with_reported("frr1"),
        &active_bindings(&store),
    );
    assert_eq!(r1.status, IdentityStatus::Resolved);
    assert_eq!(r1.device_id.as_deref(), Some(dev1.device_id.as_str()));
    assert_eq!(r1.canonical_exporter_id.as_deref(), Some("exp-rtr-1"));

    let r2 = IdentityResolver::resolve(
        &obs("203.0.113.9", "bmp", "dc1")
            .with_exporter("bmp:frr2")
            .with_reported("frr2"),
        &active_bindings(&store),
    );
    assert_eq!(r2.status, IdentityStatus::Resolved);
    assert_eq!(r2.device_id.as_deref(), Some(dev2.device_id.as_str()));

    // A third exporter from the same NAT address matches neither
    // binding's reported identity — fail closed, no IP-based merge.
    let r3 = IdentityResolver::resolve(
        &obs("203.0.113.9", "bmp", "dc1")
            .with_exporter("bmp:frr3")
            .with_reported("frr3"),
        &active_bindings(&store),
    );
    assert!(!r3.may_update_authoritative_state());
    assert!(r3.device_id.is_none());
}

// ── D. Identical reported hostname ───────────────────────────────────────────

#[test]
fn identical_reported_hostname_does_not_merge() {
    let (_dir, store) = open_store();
    let dev1 = store
        .upsert_canonical_device(&device("10.1.0.1", Some("site-x")), "test")
        .unwrap();
    let dev2 = store
        .upsert_canonical_device(&device("10.2.0.1", Some("site-x")), "test")
        .unwrap();

    // Two exporters report the same sysName. Bindings pin transport so
    // the shared hostname alone cannot decide identity.
    let mut b1 = binding("site-x", "bmp", "exp-1", &dev1.device_id);
    b1.match_transport = Some("10.9.9.1".to_string());
    b1.match_reported_identity = Some("same-host".to_string());
    store.upsert_exporter_binding(&b1, "test").unwrap();
    let mut b2 = binding("site-x", "bmp", "exp-2", &dev2.device_id);
    b2.match_transport = Some("10.9.9.2".to_string());
    b2.match_reported_identity = Some("same-host".to_string());
    store.upsert_exporter_binding(&b2, "test").unwrap();

    let r1 = IdentityResolver::resolve(
        &obs("10.9.9.1", "bmp", "site-x")
            .with_exporter("bmp:same-host")
            .with_reported("same-host"),
        &active_bindings(&store),
    );
    assert_eq!(r1.status, IdentityStatus::Resolved);
    assert_eq!(r1.device_id.as_deref(), Some(dev1.device_id.as_str()));

    let r2 = IdentityResolver::resolve(
        &obs("10.9.9.2", "bmp", "site-x")
            .with_exporter("bmp:same-host")
            .with_reported("same-host"),
        &active_bindings(&store),
    );
    assert_eq!(r2.status, IdentityStatus::Resolved);
    assert_eq!(r2.device_id.as_deref(), Some(dev2.device_id.as_str()));

    // Same hostname from an unprovisioned transport: no trusted merge.
    let r3 = IdentityResolver::resolve(
        &obs("10.9.9.3", "bmp", "site-x")
            .with_exporter("bmp:same-host")
            .with_reported("same-host"),
        &active_bindings(&store),
    );
    assert!(!r3.may_update_authoritative_state());
    assert!(r3.device_id.is_none());
}

// ── E. Missing identity binding ──────────────────────────────────────────────

#[test]
fn unbound_exporter_is_provisional_or_unknown() {
    let (_dir, store) = open_store();
    let bindings = active_bindings(&store);
    assert!(bindings.is_empty());

    // Protocol/reported evidence but no binding → Provisional, no device_id.
    let with_evidence = obs("198.51.100.7", "bmp", "site-z")
        .with_exporter("bmp:new-device")
        .with_reported("new-device");
    let r = IdentityResolver::resolve(&with_evidence, &bindings);
    assert_eq!(r.status, IdentityStatus::Provisional);
    assert!(r.device_id.is_none());
    assert!(!r.may_update_authoritative_state());

    // Bare transport observation, no protocol → Unknown.
    let bare = TelemetrySourceIdentity::observed("198.51.100.8");
    let r2 = IdentityResolver::resolve(&bare, &bindings);
    assert_eq!(r2.status, IdentityStatus::Unknown);
    assert!(r2.device_id.is_none());
}

// ── F. Conflicting identity ──────────────────────────────────────────────────

#[test]
fn contradictory_evidence_is_conflicting() {
    let (_dir, store) = open_store();
    let dev = store
        .upsert_canonical_device(&device("10.0.0.1", Some("site-c")), "test")
        .unwrap();

    let mut b = binding("site-c", "bmp", "exp-c", &dev.device_id);
    b.match_transport = Some("10.4.4.4".to_string());
    b.match_reported_identity = Some("expected-name".to_string());
    store.upsert_exporter_binding(&b, "test").unwrap();

    // Transport matches the binding but the reported identity does not —
    // the evidence points at this binding yet contradicts it.
    let obs_conflict = obs("10.4.4.4", "bmp", "site-c")
        .with_exporter("bmp:other-name")
        .with_reported("impostor-name");
    let r = IdentityResolver::resolve(&obs_conflict, &active_bindings(&store));
    assert_eq!(r.status, IdentityStatus::Conflicting);
    assert!(r.device_id.is_none());
    assert!(!r.may_update_authoritative_state());
}

// ── G. Duplicate/overlapping binding registration ────────────────────────────

#[test]
fn conflicting_bindings_rejected_at_registration() {
    let (_dir, store) = open_store();
    let dev1 = store
        .upsert_canonical_device(&device("10.0.0.1", Some("site-g")), "test")
        .unwrap();
    let dev2 = store
        .upsert_canonical_device(&device("10.0.0.2", Some("site-g")), "test")
        .unwrap();

    let mut b1 = binding("site-g", "bmp", "exp-shared", &dev1.device_id);
    b1.match_transport = Some("10.5.5.5".to_string());
    store.upsert_exporter_binding(&b1, "test").unwrap();

    // Same canonical exporter id claimed for a different device.
    let mut dup = binding("site-g", "bmp", "exp-shared", &dev2.device_id);
    dup.match_transport = Some("10.5.5.5".to_string());
    assert!(store.upsert_exporter_binding(&dup, "test").is_err());

    // Different exporter id but overlapping claim space → different device.
    let mut overlap = binding("site-g", "bmp", "exp-other", &dev2.device_id);
    overlap.match_transport = Some("10.5.5.5".to_string());
    assert!(store.upsert_exporter_binding(&overlap, "test").is_err());

    // Idempotent re-upsert of the same binding_id succeeds.
    let again = store
        .upsert_exporter_binding(&b1, "test")
        .expect("idempotent upsert");
    assert_eq!(again.binding_id, b1.binding_id);
    assert_eq!(active_bindings(&store).len(), 1);
}

#[test]
fn resolver_reports_ambiguous_for_duplicate_claims() {
    // Defense in depth: even if overlapping bindings exist (e.g. imported
    // data), the resolver must not first-match select.
    let dev_a = "dev-a".to_string();
    let dev_b = "dev-b".to_string();
    let mut b1 = binding("site-m", "bmp", "exp-1", &dev_a);
    b1.match_transport = Some("10.6.6.6".to_string());
    let mut b2 = binding("site-m", "bmp", "exp-2", &dev_b);
    b2.match_transport = Some("10.6.6.6".to_string());
    // Wildcard on remaining dims → both claim the same observation.
    let bindings = vec![b1, b2];

    let r = IdentityResolver::resolve(&obs("10.6.6.6", "bmp", "site-m"), &bindings);
    assert_eq!(r.status, IdentityStatus::Ambiguous);
    assert!(r.device_id.is_none());
    assert!(!r.may_update_authoritative_state());
}

// ── H. Persistence across reopen ─────────────────────────────────────────────

#[test]
fn bindings_and_devices_survive_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let device_id;
    let binding_id;
    {
        let store = SqliteStore::open(dir.path()).expect("open");
        let dev = store
            .upsert_canonical_device(&device("10.7.7.7", Some("site-p")), "test")
            .unwrap();
        device_id = dev.device_id.clone();
        let mut b = binding("site-p", "bmp", "exp-p", &device_id);
        b.match_exporter_claim = Some("bmp:persisted".to_string());
        let stored = store.upsert_exporter_binding(&b, "test").unwrap();
        binding_id = stored.binding_id.clone();
    }

    let store = SqliteStore::open(dir.path()).expect("reopen");
    let dev = store.get_canonical_device(&device_id).unwrap().unwrap();
    assert_eq!(dev.address, "10.7.7.7");
    let b = store.get_exporter_binding(&binding_id).unwrap().unwrap();
    assert_eq!(b.device_id, device_id);
    assert_eq!(b.provenance, BindingProvenance::OperatorProvisioned);
    assert_eq!(b.status, BindingStatus::Active);

    let o = obs("10.7.7.7", "bmp", "site-p").with_exporter("bmp:persisted");
    let r1 = IdentityResolver::resolve(&o, &active_bindings(&store));
    assert_eq!(r1.status, IdentityStatus::Resolved);
    assert_eq!(r1.device_id.as_deref(), Some(device_id.as_str()));
    assert_eq!(r1.binding_id.as_deref(), Some(binding_id.as_str()));
}

// ── I. Observation-domain reuse ──────────────────────────────────────────────

#[test]
fn shared_observation_domain_does_not_collide() {
    let (_dir, store) = open_store();
    let dev1 = store
        .upsert_canonical_device(&device("10.8.0.1", Some("site-i")), "test")
        .unwrap();
    let dev2 = store
        .upsert_canonical_device(&device("10.8.0.2", Some("site-i")), "test")
        .unwrap();

    // Two exporters report the SAME IPFIX observation domain id (42) —
    // ODID is not globally unique. Bindings disambiguate by transport.
    let mut b1 = binding("site-i", "ipfix", "exp-nf-1", &dev1.device_id);
    b1.match_transport = Some("10.8.0.1".to_string());
    b1.match_observation_domain = Some(42);
    store.upsert_exporter_binding(&b1, "test").unwrap();
    let mut b2 = binding("site-i", "ipfix", "exp-nf-2", &dev2.device_id);
    b2.match_transport = Some("10.8.0.2".to_string());
    b2.match_observation_domain = Some(42);
    store.upsert_exporter_binding(&b2, "test").unwrap();

    let o1 = obs("10.8.0.1", "ipfix", "site-i").with_domain(42);
    let r1 = IdentityResolver::resolve(&o1, &active_bindings(&store));
    assert_eq!(r1.status, IdentityStatus::Resolved);
    assert_eq!(r1.device_id.as_deref(), Some(dev1.device_id.as_str()));

    let o2 = obs("10.8.0.2", "ipfix", "site-i").with_domain(42);
    let r2 = IdentityResolver::resolve(&o2, &active_bindings(&store));
    assert_eq!(r2.status, IdentityStatus::Resolved);
    assert_eq!(r2.device_id.as_deref(), Some(dev2.device_id.as_str()));

    // Same ODID from an unbound exporter stays unresolved.
    let o3 = obs("10.8.0.3", "ipfix", "site-i").with_domain(42);
    let r3 = IdentityResolver::resolve(&o3, &active_bindings(&store));
    assert!(!r3.may_update_authoritative_state());
    assert!(r3.device_id.is_none());
}

// ── J. Provenance / authorization ────────────────────────────────────────────

#[test]
fn bindings_require_inventory_device_and_record_provenance() {
    let (_dir, store) = open_store();

    // Cannot bind to a nonexistent canonical device.
    let ghost = binding("site-j", "bmp", "exp-ghost", "dev-does-not-exist");
    assert!(store.upsert_exporter_binding(&ghost, "test").is_err());
    assert!(active_bindings(&store).is_empty());

    // Provisioned binding records actor + provenance.
    let dev = store
        .upsert_canonical_device(&device("10.9.0.1", Some("site-j")), "test")
        .unwrap();
    let b = binding("site-j", "bmp", "exp-j", &dev.device_id);
    let stored = store.upsert_exporter_binding(&b, "operator-alice").unwrap();
    assert_eq!(stored.provenance, BindingProvenance::OperatorProvisioned);
    assert_eq!(stored.created_by, "operator-alice");
    assert_eq!(stored.updated_by, "operator-alice");

    // Revocation is explicit and audited; revoked bindings stop claiming.
    let o = obs("10.9.0.1", "bmp", "site-j").with_exporter("exp-j");
    let before = IdentityResolver::resolve(&o, &active_bindings(&store));
    assert_eq!(before.status, IdentityStatus::Resolved);

    store
        .revoke_exporter_binding(&stored.binding_id, "operator-alice")
        .unwrap();
    let after = IdentityResolver::resolve(&o, &active_bindings(&store));
    assert_ne!(after.status, IdentityStatus::Resolved);
    assert!(after.device_id.is_none());

    // Resolver is pure: resolving does not create or mutate bindings.
    assert_eq!(active_bindings(&store).len(), 0);
    let all = store.list_exporter_bindings(None, None, true).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, BindingStatus::Revoked);
}

// ── Schema migration v2 → v3 on an existing database ─────────────────────────

#[test]
fn schema_v3_migration_preserves_existing_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bonsai_config.db");

    // Build a v3 store with data, then simulate a pre-v3 database:
    // drop the v3 tables and rewind user_version to 2.
    {
        let store = SqliteStore::open(dir.path()).expect("open");
        store
            .upsert_canonical_device(&device("10.10.0.1", Some("site-mig")), "test")
            .unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "DROP TABLE exporter_bindings; DROP TABLE canonical_devices;
             DROP INDEX IF EXISTS idx_canonical_devices_address;
             PRAGMA user_version=2;",
        )
        .unwrap();
        // Seed a legacy devices row as a pre-v3 database would have.
        conn.execute(
            "INSERT INTO devices (address, enabled, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action) VALUES ('10.10.0.9', 1, '[]', 0, 1, 1, 'seed', 'seed', 'seed')",
            [],
        )
        .unwrap();
    }

    // Reopen: migration must recreate the v3 tables and keep devices.
    let store = SqliteStore::open(dir.path()).expect("reopen migrated");
    let devices = store.list_devices().expect("list devices");
    assert!(devices.iter().any(|d| d.address == "10.10.0.9"));
    assert!(store.list_canonical_devices().unwrap().is_empty());
    assert!(active_bindings(&store).is_empty());

    // v3 features work on the migrated database.
    let dev = store
        .upsert_canonical_device(&device("10.10.0.2", Some("site-mig")), "test")
        .unwrap();
    let mut b = binding("site-mig", "bmp", "exp-mig", &dev.device_id);
    b.match_transport = Some("10.10.0.2".to_string());
    store.upsert_exporter_binding(&b, "test").unwrap();
    let r = IdentityResolver::resolve(
        &obs("10.10.0.2", "bmp", "site-mig"),
        &active_bindings(&store),
    );
    assert_eq!(r.status, IdentityStatus::Resolved);
    assert_eq!(r.device_id.as_deref(), Some(dev.device_id.as_str()));
}

// ── BMP adapter ──────────────────────────────────────────────────────────────

#[test]
fn bmp_observation_source_maps_to_identity_envelope() {
    use rbmp_core::bmp::types::ObservationSource;

    let source = ObservationSource::with_ids(
        "collector-1",
        "bmp:frr-edge",
        "203.0.113.50".parse().unwrap(),
        "default",
        7,
    );
    let identity = bonsai::telemetry_identity::identity_from_bmp_observation(
        &source,
        Some("frr-edge"),
        Some("site-bmp".to_string()),
    );

    assert_eq!(identity.transport_source, "203.0.113.50");
    assert_eq!(identity.protocol.as_deref(), Some("bmp"));
    assert_eq!(identity.exporter_id.as_deref(), Some("bmp:frr-edge"));
    assert_eq!(identity.reported_identity.as_deref(), Some("frr-edge"));
    assert_eq!(identity.scope.as_deref(), Some("site-bmp"));
    assert_eq!(identity.collector_id.as_deref(), Some("collector-1"));
    assert_eq!(identity.session_epoch, Some(7));
    assert_eq!(identity.identity_status, IdentityStatus::Unknown);
    assert!(identity.device_id.is_none());
}

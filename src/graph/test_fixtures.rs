// Test graph fixtures for graph/queries.rs tests.
//
// Topology: 2-spine / 4-leaf DC fabric, SP pair, 1 isolated device.
//
//   spine1 (10.0.0.1) ─eth1─ leaf1 (10.0.0.3)
//   spine1 (10.0.0.1) ─eth2─ leaf2 (10.0.0.4)
//   spine2 (10.0.0.2) ─eth1─ leaf1 (10.0.0.3)
//   spine2 (10.0.0.2) ─eth2─ leaf2 (10.0.0.4)
//   pe1    (10.0.0.7) ─eth1─ pe2   (10.0.0.8)
//   isolated (10.0.0.9) — no interfaces, no connections
//
// Applications:   app-web on leaf1 (RUNS_SERVICE)
//                 app-api on leaf2 (RUNS_SERVICE)
//                 app-db  on leaf1 (CARRIES_APPLICATION)
//
// Detection events:
//   det-open      on leaf1  (bgp_session_down, critical)  — no remediation
//   det-resolved  on spine1 (interface_down, warning)     — linked to rem-1
//   det-multi-1   on leaf1  (bgp_session_down, critical)  — for co-fire tests
//   det-multi-2   on leaf1  (interface_down, warning)     — for co-fire tests
//
// Enrichment (only spine1 and leaf1 have it; spine2 deliberately missing):
//   spine1: site_code=DC01 (source=netbox)
//   leaf1:  rack=rack-01   (source=netbox)
//
// Subscription status:
//   spine1: /openconfig-interfaces:interfaces → active

use std::sync::Arc;

use lbug::{Connection, Database, Value};

use crate::graph::GraphStore;
use super::common::ts;

pub const TEST_ENV_DC_ID: &str = "env-dc-test";
pub const TEST_ENV_DC_NAME: &str = "DC Prod";
pub const TEST_ENV_SP_ID: &str = "env-sp-test";
pub const TEST_SITE_DC_ID: &str = "site-dc-test";
pub const TEST_SITE_DC_NAME: &str = "dc-site-1";
pub const TEST_SITE_SP_ID: &str = "site-sp-test";

// Fixed timestamp for all "created_at / updated_at" fields; value doesn't matter for tests.
const TS: i64 = 1_700_000_000_000_000_000i64; // 2023-11-14

pub struct TestGraph {
    pub store: GraphStore,
    pub db: Arc<Database>,
    _tmpdir: tempfile::TempDir,
}

impl TestGraph {
    /// Build a fully populated test graph in a temporary directory.
    pub fn build() -> Self {
        let tmpdir = tempfile::tempdir().expect("temp dir for test graph");
        let path = tmpdir.path().join("testgraph").to_string_lossy().into_owned();
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open test graph store");
        let db = store.db();

        {
            let conn = Connection::new(&db).expect("test graph connection");
            build_environments(&conn);
            build_sites(&conn);
            build_devices(&conn);
            build_interfaces_and_links(&conn);
            build_applications(&conn);
            build_detections(&conn);
            build_enrichment(&conn);
            build_subscription_status(&conn);
        } // conn dropped before db moves into Self

        Self { store, db, _tmpdir: tmpdir }
    }
}

fn t() -> Value { ts(TS) }

// ─── environments ─────────────────────────────────────────────────────────────

fn build_environments(conn: &Connection<'_>) {
    for (id, name, arch) in [
        (TEST_ENV_DC_ID, TEST_ENV_DC_NAME, "data_center"),
        (TEST_ENV_SP_ID, "SP Core", "service_provider"),
    ] {
        let mut s = conn
            .prepare(
                "CREATE (e:Environment {id: $id, name: $name, archetype: $arch, \
                 created_at: $ts, metadata_json: '{}'})",
            )
            .expect("prepare env");
        conn.execute(
            &mut s,
            vec![
                ("id", Value::String(id.to_string())),
                ("name", Value::String(name.to_string())),
                ("arch", Value::String(arch.to_string())),
                ("ts", t()),
            ],
        )
        .expect("create environment");
    }
}

// ─── sites ────────────────────────────────────────────────────────────────────

fn build_sites(conn: &Connection<'_>) {
    for (id, name, env_id) in [
        (TEST_SITE_DC_ID, TEST_SITE_DC_NAME, TEST_ENV_DC_ID),
        (TEST_SITE_SP_ID, "sp-site-1", TEST_ENV_SP_ID),
    ] {
        let mut s = conn
            .prepare(
                "CREATE (s:Site {id: $id, name: $name, parent_id: '', kind: 'datacenter', \
                 lat: 0.0, lon: 0.0, metadata_json: '{}', updated_at: $ts})",
            )
            .expect("prepare site");
        conn.execute(
            &mut s,
            vec![
                ("id", Value::String(id.to_string())),
                ("name", Value::String(name.to_string())),
                ("ts", t()),
            ],
        )
        .expect("create site");

        let mut s = conn
            .prepare(
                "MATCH (s:Site {id: $sid}), (e:Environment {id: $eid}) \
                 CREATE (s)-[:BELONGS_TO_ENVIRONMENT]->(e)",
            )
            .expect("prepare site→env");
        conn.execute(
            &mut s,
            vec![
                ("sid", Value::String(id.to_string())),
                ("eid", Value::String(env_id.to_string())),
            ],
        )
        .expect("link site to env");
    }
}

// ─── devices ──────────────────────────────────────────────────────────────────

struct DeviceSeed {
    address: &'static str,
    hostname: &'static str,
    vendor: &'static str,
    site_id: &'static str,
}

fn build_devices(conn: &Connection<'_>) {
    let devices = vec![
        DeviceSeed { address: "10.0.0.1", hostname: "spine1",   vendor: "nokia",   site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.2", hostname: "spine2",   vendor: "nokia",   site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.3", hostname: "leaf1",    vendor: "arista",  site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.4", hostname: "leaf2",    vendor: "arista",  site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.5", hostname: "leaf3",    vendor: "cisco",   site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.6", hostname: "leaf4",    vendor: "cisco",   site_id: TEST_SITE_DC_ID },
        DeviceSeed { address: "10.0.0.7", hostname: "pe1",      vendor: "juniper", site_id: TEST_SITE_SP_ID },
        DeviceSeed { address: "10.0.0.8", hostname: "pe2",      vendor: "juniper", site_id: TEST_SITE_SP_ID },
        DeviceSeed { address: "10.0.0.9", hostname: "isolated", vendor: "nokia",   site_id: TEST_SITE_DC_ID },
    ];

    let mut dev_stmt = conn
        .prepare(
            "CREATE (d:Device {address: $addr, hostname: $hn, vendor: $vendor, updated_at: $ts})",
        )
        .expect("prepare device");
    let mut loc_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (s:Site {id: $sid}) \
             CREATE (d)-[:LOCATED_AT]->(s)",
        )
        .expect("prepare LOCATED_AT");

    for d in &devices {
        conn.execute(
            &mut dev_stmt,
            vec![
                ("addr", Value::String(d.address.to_string())),
                ("hn", Value::String(d.hostname.to_string())),
                ("vendor", Value::String(d.vendor.to_string())),
                ("ts", t()),
            ],
        )
        .expect("create device");

        conn.execute(
            &mut loc_stmt,
            vec![
                ("addr", Value::String(d.address.to_string())),
                ("sid", Value::String(d.site_id.to_string())),
            ],
        )
        .expect("LOCATED_AT");
    }
}

// ─── interfaces and topology ───────────────────────────────────────────────────

struct Link {
    a_dev: &'static str,
    a_if: &'static str,
    b_dev: &'static str,
    b_if: &'static str,
}

fn build_interfaces_and_links(conn: &Connection<'_>) {
    let links = vec![
        Link { a_dev: "10.0.0.1", a_if: "eth1", b_dev: "10.0.0.3", b_if: "eth1" },
        Link { a_dev: "10.0.0.1", a_if: "eth2", b_dev: "10.0.0.4", b_if: "eth1" },
        Link { a_dev: "10.0.0.2", a_if: "eth1", b_dev: "10.0.0.3", b_if: "eth2" },
        Link { a_dev: "10.0.0.2", a_if: "eth2", b_dev: "10.0.0.4", b_if: "eth2" },
        Link { a_dev: "10.0.0.7", a_if: "eth1", b_dev: "10.0.0.8", b_if: "eth1" },
    ];

    let mut iface_stmt = conn
        .prepare(
            "MERGE (i:Interface {id: $id}) \
             ON CREATE SET i.device_address = $addr, i.name = $name, \
               i.in_pkts = 0, i.out_pkts = 0, i.in_octets = 0, i.out_octets = 0, \
               i.in_errors = 0, i.out_errors = 0, i.carrier_transitions = 0, \
               i.updated_at = $ts",
        )
        .expect("prepare interface");
    let mut hi_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) \
             CREATE (d)-[:HAS_INTERFACE]->(i)",
        )
        .expect("prepare HAS_INTERFACE");
    let mut ct_stmt = conn
        .prepare(
            "MATCH (a:Interface {id: $aid}), (b:Interface {id: $bid}) \
             CREATE (a)-[:CONNECTED_TO]->(b)",
        )
        .expect("prepare CONNECTED_TO");

    for lnk in &links {
        let aid = format!("{}-{}", lnk.a_dev, lnk.a_if);
        let bid = format!("{}-{}", lnk.b_dev, lnk.b_if);

        for (id, addr, name) in [
            (aid.as_str(), lnk.a_dev, lnk.a_if),
            (bid.as_str(), lnk.b_dev, lnk.b_if),
        ] {
            conn.execute(
                &mut iface_stmt,
                vec![
                    ("id", Value::String(id.to_string())),
                    ("addr", Value::String(addr.to_string())),
                    ("name", Value::String(name.to_string())),
                    ("ts", t()),
                ],
            )
            .expect("create interface");

            conn.execute(
                &mut hi_stmt,
                vec![
                    ("addr", Value::String(addr.to_string())),
                    ("id", Value::String(id.to_string())),
                ],
            )
            .expect("HAS_INTERFACE");
        }

        conn.execute(
            &mut ct_stmt,
            vec![
                ("aid", Value::String(aid.clone())),
                ("bid", Value::String(bid.clone())),
            ],
        )
        .expect("CONNECTED_TO");
    }
}

// ─── applications ─────────────────────────────────────────────────────────────

fn build_applications(conn: &Connection<'_>) {
    let mut app_stmt = conn
        .prepare(
            "CREATE (a:Application {id: $id, name: $name, criticality: 'high', \
             owner_group: 'platform', source_name: 'netbox', updated_at: $ts})",
        )
        .expect("prepare application");
    let mut rs_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (a:Application {id: $aid}) \
             CREATE (d)-[:RUNS_SERVICE]->(a)",
        )
        .expect("prepare RUNS_SERVICE");
    let mut ca_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (a:Application {id: $aid}) \
             CREATE (d)-[:CARRIES_APPLICATION]->(a)",
        )
        .expect("prepare CARRIES_APPLICATION");

    for (id, name) in [("app-web", "app-web"), ("app-api", "app-api"), ("app-db", "app-db")] {
        conn.execute(
            &mut app_stmt,
            vec![
                ("id", Value::String(id.to_string())),
                ("name", Value::String(name.to_string())),
                ("ts", t()),
            ],
        )
        .expect("create application");
    }

    conn.execute(
        &mut rs_stmt,
        vec![
            ("addr", Value::String("10.0.0.3".to_string())),
            ("aid", Value::String("app-web".to_string())),
        ],
    )
    .expect("leaf1 RUNS_SERVICE app-web");

    conn.execute(
        &mut ca_stmt,
        vec![
            ("addr", Value::String("10.0.0.3".to_string())),
            ("aid", Value::String("app-db".to_string())),
        ],
    )
    .expect("leaf1 CARRIES_APPLICATION app-db");

    conn.execute(
        &mut rs_stmt,
        vec![
            ("addr", Value::String("10.0.0.4".to_string())),
            ("aid", Value::String("app-api".to_string())),
        ],
    )
    .expect("leaf2 RUNS_SERVICE app-api");
}

// ─── detections ───────────────────────────────────────────────────────────────

fn build_detections(conn: &Connection<'_>) {
    let mut det_stmt = conn
        .prepare(
            "CREATE (de:DetectionEvent {id: $id, device_address: $addr, \
             rule_id: $rule, severity: $sev, features_json: '{}', fired_at: $ts})",
        )
        .expect("prepare DetectionEvent");
    let mut trig_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (de:DetectionEvent {id: $did}) \
             CREATE (d)-[:TRIGGERED]->(de)",
        )
        .expect("prepare TRIGGERED");

    let events = vec![
        ("det-open",     "10.0.0.3", "bgp_session_down", "critical", TS + 3_600_000_000_000i64),
        ("det-resolved", "10.0.0.1", "interface_down",   "warning",  TS + 1_800_000_000_000i64),
        ("det-multi-1",  "10.0.0.3", "bgp_session_down", "critical", TS + 7_200_000_000_000i64),
        ("det-multi-2",  "10.0.0.3", "interface_down",   "warning",  TS + 7_260_000_000_000i64),
    ];

    for (id, addr, rule, sev, fired_at_ns) in &events {
        conn.execute(
            &mut det_stmt,
            vec![
                ("id", Value::String(id.to_string())),
                ("addr", Value::String(addr.to_string())),
                ("rule", Value::String(rule.to_string())),
                ("sev", Value::String(sev.to_string())),
                ("ts", ts(*fired_at_ns)),
            ],
        )
        .expect("create DetectionEvent");

        conn.execute(
            &mut trig_stmt,
            vec![
                ("addr", Value::String(addr.to_string())),
                ("did", Value::String(id.to_string())),
            ],
        )
        .expect("TRIGGERED");
    }

    // rem-1 resolves det-resolved
    let mut rem_stmt = conn
        .prepare(
            "CREATE (r:Remediation {id: $id, detection_id: $did, \
             action: 'no_shut_interface', status: 'success', detail_json: '{}', \
             attempted_at: $ts, completed_at: $ts})",
        )
        .expect("prepare Remediation");
    conn.execute(
        &mut rem_stmt,
        vec![
            ("id", Value::String("rem-1".to_string())),
            ("did", Value::String("det-resolved".to_string())),
            ("ts", t()),
        ],
    )
    .expect("create rem-1");

    let mut resolves_stmt = conn
        .prepare(
            "MATCH (r:Remediation {id: $rid}), (de:DetectionEvent {id: $did}) \
             CREATE (r)-[:RESOLVES]->(de)",
        )
        .expect("prepare RESOLVES");
    conn.execute(
        &mut resolves_stmt,
        vec![
            ("rid", Value::String("rem-1".to_string())),
            ("did", Value::String("det-resolved".to_string())),
        ],
    )
    .expect("RESOLVES");
}

// ─── enrichment ───────────────────────────────────────────────────────────────

fn build_enrichment(conn: &Connection<'_>) {
    let mut ep_stmt = conn
        .prepare(
            "CREATE (ep:EnrichmentProperty {id: $id, device_address: $addr, \
             key: $key, value: $val, source_name: $src, updated_at: $ts})",
        )
        .expect("prepare EnrichmentProperty");
    let mut link_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (ep:EnrichmentProperty {id: $eid}) \
             CREATE (d)-[:HAS_ENRICHMENT_PROPERTY]->(ep)",
        )
        .expect("prepare HAS_ENRICHMENT_PROPERTY");

    // spine1 enriched; spine2 deliberately NOT enriched
    let props = vec![
        ("ep-spine1-1", "10.0.0.1", "site_code", "DC01",    "netbox"),
        ("ep-leaf1-1",  "10.0.0.3", "rack",       "rack-01", "netbox"),
    ];
    for (id, addr, key, val, src) in &props {
        conn.execute(
            &mut ep_stmt,
            vec![
                ("id", Value::String(id.to_string())),
                ("addr", Value::String(addr.to_string())),
                ("key", Value::String(key.to_string())),
                ("val", Value::String(val.to_string())),
                ("src", Value::String(src.to_string())),
                ("ts", t()),
            ],
        )
        .expect("create EnrichmentProperty");

        conn.execute(
            &mut link_stmt,
            vec![
                ("addr", Value::String(addr.to_string())),
                ("eid", Value::String(id.to_string())),
            ],
        )
        .expect("HAS_ENRICHMENT_PROPERTY");
    }
}

// ─── subscription status ──────────────────────────────────────────────────────

fn build_subscription_status(conn: &Connection<'_>) {
    let mut ss_stmt = conn
        .prepare(
            "CREATE (ss:SubscriptionStatus { \
             id: $id, device_address: $addr, \
             path: $path, origin: 'openconfig', mode: 'SAMPLE', \
             sample_interval_ns: 10000000000, status: 'active', \
             first_observed_at: $ts, last_observed_at: $ts, updated_at: $ts})",
        )
        .expect("prepare SubscriptionStatus");
    conn.execute(
        &mut ss_stmt,
        vec![
            ("id", Value::String("ss-spine1-iface".to_string())),
            ("addr", Value::String("10.0.0.1".to_string())),
            ("path", Value::String("/openconfig-interfaces:interfaces".to_string())),
            ("ts", t()),
        ],
    )
    .expect("create SubscriptionStatus");

    let mut link_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (ss:SubscriptionStatus {id: $ssid}) \
             CREATE (d)-[:HAS_SUBSCRIPTION_STATUS]->(ss)",
        )
        .expect("prepare HAS_SUBSCRIPTION_STATUS");
    conn.execute(
        &mut link_stmt,
        vec![
            ("addr", Value::String("10.0.0.1".to_string())),
            ("ssid", Value::String("ss-spine1-iface".to_string())),
        ],
    )
    .expect("HAS_SUBSCRIPTION_STATUS");
}

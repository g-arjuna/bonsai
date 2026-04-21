use lbug::{Connection, Database, SystemConfig};

fn main() {
    let db_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bonsai-mv.db".to_string());
    let db = Database::new(&db_path, SystemConfig::default())
        .unwrap_or_else(|_| panic!("failed to open {db_path} — run from project root"));
    let conn = Connection::new(&db).unwrap();

    println!("\n=== DEVICES ===");
    let r = conn
        .query("MATCH (d:Device) RETURN d.address, d.vendor, d.updated_at")
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== INTERFACES (sample: 5) ===");
    let r = conn
        .query(
            "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) \
         RETURN d.address, i.name, i.in_pkts, i.out_pkts, i.in_octets LIMIT 5",
        )
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== BGP NEIGHBORS ===");
    let r = conn
        .query(
            "MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) \
         RETURN d.address, n.peer_address, n.peer_as, n.session_state",
        )
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== LLDP NEIGHBORS ===");
    let r = conn
        .query(
            "MATCH (d:Device)-[:HAS_LLDP_NEIGHBOR]->(n:LldpNeighbor) \
         RETURN d.address, n.local_if, n.chassis_id, n.system_name, n.port_id",
        )
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== BGP FLAPS (crpd peer 10.1.31.0, last 20) ===");
    let r = conn
        .query(
            "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) \
         WHERE e.detail CONTAINS '10.1.31.0' OR e.detail CONTAINS '10.1.23.1' \
         RETURN d.address, e.detail, e.occurred_at \
         ORDER BY e.occurred_at DESC LIMIT 20",
        )
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== STATE CHANGE EVENTS (last 10) ===");
    let r = conn
        .query(
            "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) \
         RETURN d.address, e.event_type, e.detail, e.occurred_at \
         ORDER BY e.occurred_at DESC LIMIT 10",
        )
        .unwrap();
    for row in r {
        println!("  {:?}", row);
    }

    println!("\n=== COUNTS ===");
    for (label, q) in [
        ("devices", "MATCH (n:Device) RETURN count(n)"),
        ("interfaces", "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors", "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("lldp-neighbors", "MATCH (n:LldpNeighbor) RETURN count(n)"),
        (
            "state-change-events",
            "MATCH (n:StateChangeEvent) RETURN count(n)",
        ),
        (
            "HAS_INTERFACE edges",
            "MATCH ()-[r:HAS_INTERFACE]->() RETURN count(r)",
        ),
        (
            "PEERS_WITH edges",
            "MATCH ()-[r:PEERS_WITH]->() RETURN count(r)",
        ),
        (
            "HAS_LLDP_NEIGHBOR edges",
            "MATCH ()-[r:HAS_LLDP_NEIGHBOR]->() RETURN count(r)",
        ),
        (
            "REPORTED_BY edges",
            "MATCH ()-[r:REPORTED_BY]->() RETURN count(r)",
        ),
        (
            "CONNECTED_TO edges",
            "MATCH ()-[r:CONNECTED_TO]->() RETURN count(r)",
        ),
    ] {
        let mut r = conn.query(q).unwrap();
        if let Some(row) = r.next() {
            println!("  {}: {:?}", label, row[0]);
        }
    }
}

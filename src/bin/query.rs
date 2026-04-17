use lbug::{Connection, Database, SystemConfig};

fn main() {
    let db = Database::new("bonsai.db", SystemConfig::default())
        .expect("failed to open bonsai.db — run from project root");
    let conn = Connection::new(&db).unwrap();

    println!("\n=== DEVICES ===");
    let mut r = conn.query("MATCH (d:Device) RETURN d.address, d.vendor, d.updated_at").unwrap();
    while let Some(row) = r.next() {
        println!("  {:?}", row);
    }

    println!("\n=== INTERFACES (sample: 5) ===");
    let mut r = conn.query(
        "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) \
         RETURN d.address, i.name, i.in_pkts, i.out_pkts, i.in_octets LIMIT 5"
    ).unwrap();
    while let Some(row) = r.next() {
        println!("  {:?}", row);
    }

    println!("\n=== BGP NEIGHBORS ===");
    let mut r = conn.query(
        "MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) \
         RETURN d.address, n.peer_address, n.peer_as, n.session_state"
    ).unwrap();
    while let Some(row) = r.next() {
        println!("  {:?}", row);
    }

    println!("\n=== COUNTS ===");
    for (label, q) in [
        ("devices",    "MATCH (n:Device) RETURN count(n)"),
        ("interfaces", "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors", "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("HAS_INTERFACE edges", "MATCH ()-[r:HAS_INTERFACE]->() RETURN count(r)"),
        ("PEERS_WITH edges",    "MATCH ()-[r:PEERS_WITH]->() RETURN count(r)"),
    ] {
        let mut r = conn.query(q).unwrap();
        if let Some(row) = r.next() {
            println!("  {}: {:?}", label, row[0]);
        }
    }
}

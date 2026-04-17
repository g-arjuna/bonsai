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

    println!("\n=== LLDP NEIGHBORS ===");
    let mut r = conn.query(
        "MATCH (d:Device)-[:HAS_LLDP_NEIGHBOR]->(n:LldpNeighbor) \
         RETURN d.address, n.local_if, n.chassis_id, n.system_name, n.port_id"
    ).unwrap();
    while let Some(row) = r.next() {
        println!("  {:?}", row);
    }

    println!("\n=== STATE CHANGE EVENTS (last 10) ===");
    let mut r = conn.query(
        "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) \
         RETURN d.address, e.event_type, e.detail, e.occurred_at \
         ORDER BY e.occurred_at DESC LIMIT 10"
    ).unwrap();
    while let Some(row) = r.next() {
        println!("  {:?}", row);
    }

    println!("\n=== COUNTS ===");
    for (label, q) in [
        ("devices",             "MATCH (n:Device) RETURN count(n)"),
        ("interfaces",          "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors",       "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("lldp-neighbors",      "MATCH (n:LldpNeighbor) RETURN count(n)"),
        ("state-change-events", "MATCH (n:StateChangeEvent) RETURN count(n)"),
        ("HAS_INTERFACE edges",    "MATCH ()-[r:HAS_INTERFACE]->() RETURN count(r)"),
        ("PEERS_WITH edges",       "MATCH ()-[r:PEERS_WITH]->() RETURN count(r)"),
        ("HAS_LLDP_NEIGHBOR edges","MATCH ()-[r:HAS_LLDP_NEIGHBOR]->() RETURN count(r)"),
        ("REPORTED_BY edges",      "MATCH ()-[r:REPORTED_BY]->() RETURN count(r)"),
    ] {
        let mut r = conn.query(q).unwrap();
        if let Some(row) = r.next() {
            println!("  {}: {:?}", label, row[0]);
        }
    }
}

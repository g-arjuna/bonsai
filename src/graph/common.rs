use anyhow::{Context, Result};
use lbug::{Connection, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

pub fn ts(ns: i64) -> Value {
    let dt = OffsetDateTime::UNIX_EPOCH + time::Duration::nanoseconds(ns);
    Value::TimestampNs(dt)
}

pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

pub fn upsert_device(
    conn: &Connection<'_>,
    address: &str,
    vendor: &str,
    hostname: &str,
    role: &str,
    site: &str,
    now: Value,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (d:Device {address: $addr}) \
         ON CREATE SET d.vendor = $vendor, d.hostname = $hn, d.updated_at = $ts \
         ON MATCH SET \
           d.vendor = CASE WHEN $vendor <> '' THEN $vendor ELSE d.vendor END, \
           d.hostname = CASE WHEN $hn <> '' THEN $hn ELSE d.hostname END, \
           d.updated_at = $ts",
        )
        .context("prepare Device upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("addr", Value::String(address.to_string())),
            ("vendor", Value::String(vendor.to_string())),
            ("hn", Value::String(hostname.to_string())),
            ("ts", now),
        ],
    )
    .context("execute Device upsert")?;

    // role and site are written with plain SET to avoid lbug binder errors
    // when reading a property that doesn't yet exist in the graph schema.
    // Callers that don't know these values pass "" and we skip.
    if !role.is_empty() {
        let mut s = conn
            .prepare("MATCH (d:Device {address: $addr}) SET d.role = $role")
            .context("prepare Device role SET")?;
        conn.execute(
            &mut s,
            vec![
                ("addr", Value::String(address.to_string())),
                ("role", Value::String(role.to_string())),
            ],
        )
        .context("execute Device role SET")?;
    }
    if !site.is_empty() {
        let mut s = conn
            .prepare("MATCH (d:Device {address: $addr}) SET d.site = $site")
            .context("prepare Device site SET")?;
        conn.execute(
            &mut s,
            vec![
                ("addr", Value::String(address.to_string())),
                ("site", Value::String(site.to_string())),
            ],
        )
        .context("execute Device site SET")?;
    }

    Ok(())
}

pub fn upsert_rack(
    conn: &Connection<'_>,
    rack_name: &str,
    site: &str,
    device_address: &str,
    now_ns: i64,
) -> Result<()> {
    let now = ts(now_ns);
    let mut stmt = conn
        .prepare(
            "MERGE (r:Rack {name: $name}) \
             ON CREATE SET r.site = $site, r.row_id = '', r.metadata = '', r.updated_at = $ts \
             ON MATCH SET r.site = CASE WHEN $site <> '' THEN $site ELSE r.site END, r.updated_at = $ts",
        )
        .context("prepare Rack upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("name", Value::String(rack_name.to_string())),
            ("site", Value::String(site.to_string())),
            ("ts", now),
        ],
    )
    .context("execute Rack upsert")?;
    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (r:Rack {name: $rack}) \
             MERGE (d)-[:RACK_MEMBER]->(r)",
        )
        .context("prepare RACK_MEMBER merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("rack", Value::String(rack_name.to_string())),
        ],
    )
    .context("execute RACK_MEMBER merge")?;
    Ok(())
}

pub fn upsert_optical_channel(
    conn: &Connection<'_>,
    id: &str,
    device_address: &str,
    name: &str,
    rx_power_dbm: f64,
    tx_power_dbm: f64,
    osnr_db: f64,
    pre_fec_ber: f64,
    laser_bias_ma: f64,
    temperature_c: f64,
    now_ns: i64,
) -> Result<()> {
    let now = ts(now_ns);
    let mut stmt = conn
        .prepare(
            "MERGE (c:OpticalChannel {id: $id}) \
             ON CREATE SET c.device_address = $addr, c.name = $name, \
               c.rx_power_dbm = $rx, c.tx_power_dbm = $tx, c.osnr_db = $osnr, \
               c.pre_fec_ber = $ber, c.laser_bias_ma = $bias, c.temperature_c = $temp, \
               c.updated_at = $ts \
             ON MATCH SET c.rx_power_dbm = $rx, c.tx_power_dbm = $tx, c.osnr_db = $osnr, \
               c.pre_fec_ber = $ber, c.laser_bias_ma = $bias, c.temperature_c = $temp, \
               c.updated_at = $ts",
        )
        .context("prepare OpticalChannel upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("addr", Value::String(device_address.to_string())),
            ("name", Value::String(name.to_string())),
            ("rx", Value::Double(rx_power_dbm)),
            ("tx", Value::Double(tx_power_dbm)),
            ("osnr", Value::Double(osnr_db)),
            ("ber", Value::Double(pre_fec_ber)),
            ("bias", Value::Double(laser_bias_ma)),
            ("temp", Value::Double(temperature_c)),
            ("ts", now),
        ],
    )
    .context("execute OpticalChannel upsert")?;
    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (c:OpticalChannel {id: $id}) \
             MERGE (d)-[:HAS_OPTICAL_CHANNEL]->(c)",
        )
        .context("prepare HAS_OPTICAL_CHANNEL merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("id", Value::String(id.to_string())),
        ],
    )
    .context("execute HAS_OPTICAL_CHANNEL merge")?;
    Ok(())
}

pub fn upsert_app_flow(
    conn: &Connection<'_>,
    id: &str,
    src_address: &str,
    dst_address: &str,
    dst_port: i64,
    protocol: &str,
    bytes_per_sec: f64,
    packets_per_sec: f64,
    now_ns: i64,
) -> Result<()> {
    let now = ts(now_ns);
    let mut stmt = conn
        .prepare(
            "MERGE (f:AppFlow {id: $id}) \
             ON CREATE SET f.src_address = $src, f.dst_address = $dst, f.dst_port = $port, \
               f.protocol = $proto, f.bytes_per_sec = $bps, f.packets_per_sec = $pps, \
               f.updated_at = $ts \
             ON MATCH SET f.bytes_per_sec = $bps, f.packets_per_sec = $pps, f.updated_at = $ts",
        )
        .context("prepare AppFlow upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("src", Value::String(src_address.to_string())),
            ("dst", Value::String(dst_address.to_string())),
            ("port", Value::Int64(dst_port)),
            ("proto", Value::String(protocol.to_string())),
            ("bps", Value::Double(bytes_per_sec)),
            ("pps", Value::Double(packets_per_sec)),
            ("ts", now),
        ],
    )
    .context("execute AppFlow upsert")?;
    Ok(())
}

pub fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

pub fn read_ts_ns(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

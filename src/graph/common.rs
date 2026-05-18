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
    // Stable id: "rack:{site}:{name}" — unique across multi-site deployments.
    let rack_id = format!("rack:{}:{}", site, rack_name);
    let mut stmt = conn
        .prepare(
            "MERGE (r:Rack {id: $id}) \
             ON CREATE SET r.name = $name, r.site = $site, r.row_id = '', r.metadata = '', r.updated_at = $ts \
             ON MATCH SET r.site = CASE WHEN $site <> '' THEN $site ELSE r.site END, r.updated_at = $ts",
        )
        .context("prepare Rack upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(rack_id.clone())),
            ("name", Value::String(rack_name.to_string())),
            ("site", Value::String(site.to_string())),
            ("ts", now),
        ],
    )
    .context("execute Rack upsert")?;
    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (r:Rack {id: $rack_id}) \
             MERGE (d)-[:RACK_MEMBER]->(r)",
        )
        .context("prepare RACK_MEMBER merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("rack_id", Value::String(rack_id)),
        ],
    )
    .context("execute RACK_MEMBER merge")?;
    Ok(())
}

/// Upsert a sub-site Location node (AZ, pod, building, floor, etc.) and:
/// 1. Link it to its parent Site via IN_SITE
/// 2. Link the device to it via IN_LOCATION
///
/// `site_id` must be the graph Site id (not the site name).
/// `kind` should be one of: "az", "pod", "building", "floor", "room", "other".
pub fn upsert_location(
    conn: &Connection<'_>,
    location_name: &str,
    kind: &str,
    site_id: &str,
    device_address: &str,
    source: &str,
    now_ns: i64,
) -> Result<()> {
    let now = ts(now_ns);
    let loc_id = format!("loc:{}:{}", site_id, location_name);

    let mut stmt = conn
        .prepare(
            "MERGE (l:Location {id: $id}) \
             ON CREATE SET l.name = $name, l.kind = $kind, l.site_id = $site_id, \
                           l.source = $src, l.updated_at = $ts \
             ON MATCH SET l.kind = CASE WHEN $kind <> '' THEN $kind ELSE l.kind END, \
                          l.updated_at = $ts",
        )
        .context("prepare Location upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id",      Value::String(loc_id.clone())),
            ("name",    Value::String(location_name.to_string())),
            ("kind",    Value::String(kind.to_string())),
            ("site_id", Value::String(site_id.to_string())),
            ("src",     Value::String(source.to_string())),
            ("ts",      now.clone()),
        ],
    )
    .context("execute Location upsert")?;

    // IN_SITE: Location → Site (idempotent)
    let mut in_site = conn
        .prepare(
            "MATCH (l:Location {id: $lid}), (s:Site {id: $sid}) \
             MERGE (l)-[:IN_SITE]->(s)",
        )
        .context("prepare IN_SITE merge")?;
    conn.execute(
        &mut in_site,
        vec![
            ("lid", Value::String(loc_id.clone())),
            ("sid", Value::String(site_id.to_string())),
        ],
    )
    .context("execute IN_SITE merge")?;

    // IN_LOCATION: Device → Location (idempotent; clear old before re-linking)
    let mut clear = conn
        .prepare(
            "MATCH (d:Device {address: $addr})-[r:IN_LOCATION]->(:Location) DELETE r",
        )
        .context("prepare IN_LOCATION clear")?;
    conn.execute(
        &mut clear,
        vec![("addr", Value::String(device_address.to_string()))],
    )
    .context("execute IN_LOCATION clear")?;

    let mut in_loc = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (l:Location {id: $lid}) \
             MERGE (d)-[:IN_LOCATION]->(l)",
        )
        .context("prepare IN_LOCATION merge")?;
    conn.execute(
        &mut in_loc,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("lid",  Value::String(loc_id)),
        ],
    )
    .context("execute IN_LOCATION merge")?;

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
    exporter_address: &str,
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
             ON CREATE SET f.exporter_address = $exp, f.src_address = $src, f.dst_address = $dst, \
               f.dst_port = $port, f.protocol = $proto, f.bytes_per_sec = $bps, \
               f.packets_per_sec = $pps, f.updated_at = $ts \
             ON MATCH SET f.exporter_address = $exp, f.bytes_per_sec = $bps, \
               f.packets_per_sec = $pps, f.updated_at = $ts",
        )
        .context("prepare AppFlow upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id",  Value::String(id.to_string())),
            ("exp", Value::String(exporter_address.to_string())),
            ("src", Value::String(src_address.to_string())),
            ("dst", Value::String(dst_address.to_string())),
            ("port", Value::Int64(dst_port)),
            ("proto", Value::String(protocol.to_string())),
            ("bps", Value::Double(bytes_per_sec)),
            ("pps", Value::Double(packets_per_sec)),
            ("ts",  now),
        ],
    )
    .context("execute AppFlow upsert")?;
    Ok(())
}

/// Track B2 — upsert a HostEndpoint node (server, AP client, phone, CPE, etc.).
///
/// Key: `ip` (the primary management / data-plane IP of the endpoint).
/// `kind` is optional and drives display only — set to "" to leave the
/// existing kind unchanged on match (defaults to "unknown" on create).
/// The function is a silent no-op when `ip` is empty.
pub fn upsert_host_endpoint(
    conn: &Connection<'_>,
    ip: &str,
    kind: &str,
    hostname: &str,
    mac: &str,
    vendor: &str,
    rack_id: &str,
    site_id: &str,
    source: &str,
    now_ns: i64,
) -> Result<()> {
    if ip.is_empty() {
        return Ok(());
    }
    let now = ts(now_ns);
    let effective_kind = if kind.is_empty() { "unknown" } else { kind };

    let mut stmt = conn
        .prepare(
            "MERGE (h:HostEndpoint {id: $id}) \
             ON CREATE SET h.ip = $ip, h.kind = $kind, h.hostname = $hostname, \
               h.mac = $mac, h.vendor = $vendor, h.rack_id = $rack_id, \
               h.site_id = $site_id, h.source = $src, h.updated_at = $ts \
             ON MATCH SET \
               h.kind     = CASE WHEN $kind     <> '' THEN $kind     ELSE h.kind     END, \
               h.hostname = CASE WHEN $hostname <> '' THEN $hostname ELSE h.hostname END, \
               h.mac      = CASE WHEN $mac      <> '' THEN $mac      ELSE h.mac      END, \
               h.vendor   = CASE WHEN $vendor   <> '' THEN $vendor   ELSE h.vendor   END, \
               h.rack_id  = CASE WHEN $rack_id  <> '' THEN $rack_id  ELSE h.rack_id  END, \
               h.site_id  = CASE WHEN $site_id  <> '' THEN $site_id  ELSE h.site_id  END, \
               h.updated_at = $ts",
        )
        .context("prepare HostEndpoint upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id",       Value::String(ip.to_string())),
            ("ip",       Value::String(ip.to_string())),
            ("kind",     Value::String(effective_kind.to_string())),
            ("hostname", Value::String(hostname.to_string())),
            ("mac",      Value::String(mac.to_string())),
            ("vendor",   Value::String(vendor.to_string())),
            ("rack_id",  Value::String(rack_id.to_string())),
            ("site_id",  Value::String(site_id.to_string())),
            ("src",      Value::String(source.to_string())),
            ("ts",       now),
        ],
    )
    .context("execute HostEndpoint upsert")?;
    Ok(())
}

/// Link a HostEndpoint to the Interface it is physically connected to.
/// `interface_id` uses the standard Interface PK format: "{device_address}:{if_name}".
/// Silent no-op if either node doesn't exist yet.
pub fn link_host_endpoint_to_interface(
    conn: &Connection<'_>,
    host_ip: &str,
    interface_id: &str,
) -> Result<()> {
    if host_ip.is_empty() || interface_id.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "MATCH (h:HostEndpoint {id: $hid}), (i:Interface {id: $iid}) \
             MERGE (h)-[:CONNECTED_TO]->(i)",
        )
        .context("prepare CONNECTED_TO merge")?;
    let _ = conn.execute(
        &mut stmt,
        vec![
            ("hid", Value::String(host_ip.to_string())),
            ("iid", Value::String(interface_id.to_string())),
        ],
    );
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

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
        conn.execute(&mut s, vec![
            ("addr", Value::String(address.to_string())),
            ("role", Value::String(role.to_string())),
        ])
        .context("execute Device role SET")?;
    }
    if !site.is_empty() {
        let mut s = conn
            .prepare("MATCH (d:Device {address: $addr}) SET d.site = $site")
            .context("prepare Device site SET")?;
        conn.execute(&mut s, vec![
            ("addr", Value::String(address.to_string())),
            ("site", Value::String(site.to_string())),
        ])
        .context("execute Device site SET")?;
    }

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

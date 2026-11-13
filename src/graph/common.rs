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

/// Upsert a Device node keyed on the **bare IP** (no port). The gNMI endpoint
/// (which may include a port, e.g. "10.0.0.1:57400") is stored as `gnmi_endpoint`
/// for reference but is not the identity key.
///
/// `address` MUST already be the stripped bare IP — call
/// `crate::registry::strip_port(raw_address)` before passing here.
pub fn upsert_device(
    conn: &Connection<'_>,
    address: &str,
    vendor: &str,
    hostname: &str,
    role: &str,
    site: &str,
    now: Value,
) -> Result<()> {
    let bare = crate::registry::strip_port(address);
    let endpoint = if bare != address { address } else { "" };
    upsert_device_with_endpoint(conn, bare, endpoint, vendor, hostname, role, site, now)
}

/// Like `upsert_device` but also records the gNMI endpoint (host:port).
pub fn upsert_device_with_endpoint(
    conn: &Connection<'_>,
    address: &str,
    gnmi_endpoint: &str,
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
            ("ts", now.clone()),
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
    // Store gnmi_endpoint (host:port) when provided — it differs from address when port is set.
    if !gnmi_endpoint.is_empty() && gnmi_endpoint != address {
        let mut s = conn
            .prepare("MATCH (d:Device {address: $addr}) SET d.gnmi_endpoint = $ep")
            .context("prepare Device gnmi_endpoint SET")?;
        let _ = conn.execute(
            &mut s,
            vec![
                ("addr", Value::String(address.to_string())),
                ("ep", Value::String(gnmi_endpoint.to_string())),
            ],
        );
    }

    // Register the primary IP as a DeviceAddress so signal sources that only see
    // the bare IP (NetFlow, sFlow, SNMP, BMP) can resolve back to this Device.
    let _ = upsert_device_address(conn, address, "gnmi", now.clone());
    // If a separate gnmi_endpoint was provided, also register it so an exact
    // host:port lookup (legacy code paths) still finds the node.
    if !gnmi_endpoint.is_empty() && gnmi_endpoint != address {
        let ep_ip = crate::registry::strip_port(gnmi_endpoint);
        if ep_ip != address {
            let _ = upsert_device_address(conn, ep_ip, "gnmi", now.clone());
        }
    }

    // EntityIdentity: keep the normalised identity record in sync with every device upsert.
    // chassis_id is unknown at this point (learned later from LLDP).
    if !hostname.is_empty() || !address.is_empty() {
        let _ = upsert_entity_identity(conn, address, hostname, "", address, "gnmi", now_ns());
    }

    Ok(())
}

/// Register an IP address as a `DeviceAddress` node and link it to `Device {address: device_ip}`
/// via a `KNOWN_ADDRESS_OF` edge. Idempotent — safe to call on every telemetry write.
///
/// Use this wherever a signal source reveals an additional IP for a device:
/// - extra_ips from TargetConfig (onboarding discovery)
/// - BMP TCP connection source IP
/// - NetFlow/sFlow exporter IP
/// - SNMP trap source IP
/// - LLDP management-address TLV
///
/// `device_ip` must be the canonical bare IP (Device.address primary key).
/// `signal_ip` is the IP being registered (may equal `device_ip` for the primary).
/// `source` is a short tag: "gnmi", "extra_ip", "bmp_peer", "netflow", "sflow",
///                         "snmp", "syslog_peer", "lldp".
pub fn upsert_device_address(
    conn: &Connection<'_>,
    signal_ip: &str,
    source: &str,
    now: Value,
) -> Result<()> {
    if signal_ip.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "MERGE (a:DeviceAddress {ip: $ip}) \
             ON CREATE SET a.source = $src, a.updated_at = $ts \
             ON MATCH SET a.updated_at = $ts",
        )
        .context("prepare DeviceAddress upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("ip", Value::String(signal_ip.to_string())),
            ("src", Value::String(source.to_string())),
            ("ts", now),
        ],
    )
    .context("execute DeviceAddress upsert")?;
    Ok(())
}

/// Link a DeviceAddress node to its owning Device via KNOWN_ADDRESS_OF.
/// Called once the Device node is guaranteed to exist.
/// Silent no-op if either node is missing.
pub fn link_device_address(conn: &Connection<'_>, signal_ip: &str, device_ip: &str) -> Result<()> {
    if signal_ip.is_empty() || device_ip.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "MATCH (a:DeviceAddress {ip: $sip}), (d:Device {address: $dip}) \
             MERGE (a)-[:KNOWN_ADDRESS_OF]->(d)",
        )
        .context("prepare KNOWN_ADDRESS_OF merge")?;
    if let Err(e) = conn.execute(
        &mut stmt,
        vec![
            ("sip", Value::String(signal_ip.to_string())),
            ("dip", Value::String(device_ip.to_string())),
        ],
    ) {
        tracing::debug!(error = %e, signal_ip, device_ip, "KNOWN_ADDRESS_OF edge skipped");
        return Ok(());
    }
    // Ownership evidence for `signal_ip` just changed: any stored
    // BGP_SESSION_WITH edge projecting a session that names this address
    // must be re-resolved now — not whenever the next BGP update happens
    // to arrive (which may be never for an idle session).
    if let Err(e) = reconcile_bgp_session_edges_for_peer(conn, signal_ip) {
        tracing::warn!(error = %e, signal_ip, "BGP edge re-resolution after alias link failed");
    }
    Ok(())
}

/// Create or update an EntityIdentity node and wire it to the Device.
/// Call this from any code path that learns about a device identity:
///  - `upsert_device` (gNMI path — knows address + hostname)
///  - LLDP neighbor write (knows chassis_id + system_name)
/// The node is keyed on hostname when available; falls back to chassis_id or address.
pub fn upsert_entity_identity(
    conn: &Connection<'_>,
    device_address: &str,
    hostname: &str,
    chassis_id: &str,
    mgmt_ip: &str,
    source: &str,
    now_ns: i64,
) -> Result<()> {
    // Stable key: prefer hostname, then chassis_id (MAC-based), then address.
    let key = if !hostname.is_empty() {
        hostname
    } else if !chassis_id.is_empty() {
        chassis_id
    } else {
        device_address
    };
    let id = format!("identity:{key}");
    let now = ts(now_ns);

    // Capture the previous mgmt_ip claim so a change (or first claim)
    // can trigger BGP edge re-resolution for BOTH the newly claimed and
    // the previously claimed address.
    let mut prev_stmt = conn
        .prepare("MATCH (e:EntityIdentity {id: $id}) RETURN e.mgmt_ip")
        .context("prepare EntityIdentity mgmt_ip lookup")?;
    let prev_mgmt_ip = conn
        .execute(&mut prev_stmt, vec![("id", Value::String(id.clone()))])
        .ok()
        .and_then(|mut rows| rows.next().map(|row| read_str(&row[0])))
        .unwrap_or_default();

    let mut stmt = conn
        .prepare(
            "MERGE (e:EntityIdentity {id: $id}) \
             ON CREATE SET e.hostname = $hn, e.chassis_id = $chassis, \
               e.mgmt_ip = $ip, e.source = $src, e.updated_at = $ts \
             ON MATCH SET \
               e.hostname   = CASE WHEN $hn      <> '' THEN $hn      ELSE e.hostname   END, \
               e.chassis_id = CASE WHEN $chassis <> '' THEN $chassis ELSE e.chassis_id END, \
               e.mgmt_ip    = CASE WHEN $ip      <> '' THEN $ip      ELSE e.mgmt_ip    END, \
               e.updated_at = $ts",
        )
        .context("prepare EntityIdentity upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("hn", Value::String(hostname.to_string())),
            ("chassis", Value::String(chassis_id.to_string())),
            ("ip", Value::String(mgmt_ip.to_string())),
            ("src", Value::String(source.to_string())),
            ("ts", now),
        ],
    )
    .context("execute EntityIdentity upsert")?;

    // Link Device → EntityIdentity (only when device_address is known).
    if !device_address.is_empty() {
        let mut edge = conn
            .prepare(
                "MATCH (d:Device {address: $addr}), (e:EntityIdentity {id: $id}) \
                 MERGE (d)-[:HAS_IDENTITY]->(e)",
            )
            .context("prepare HAS_IDENTITY merge")?;
        if let Err(e) = conn.execute(
            &mut edge,
            vec![
                ("addr", Value::String(device_address.to_string())),
                ("id", Value::String(id)),
            ],
        ) {
            tracing::debug!(error = %e, device_address, "HAS_IDENTITY edge skipped (device not yet written)");
        }
    }

    // mgmt_ip ownership evidence changed (new claim or reassignment):
    // re-resolve BGP session edges that name the affected addresses.
    // Gated on an actual value change so routine telemetry writes do not
    // pay for peer scans.
    if !mgmt_ip.is_empty() && mgmt_ip != prev_mgmt_ip {
        for ip in [mgmt_ip, prev_mgmt_ip.as_str()] {
            if ip.is_empty() {
                continue;
            }
            if let Err(e) = reconcile_bgp_session_edges_for_peer(conn, ip) {
                tracing::warn!(error = %e, ip, "BGP edge re-resolution after identity claim change failed");
            }
        }
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
            ("rack_id", Value::String(rack_id.clone())),
        ],
    )
    .context("execute RACK_MEMBER merge")?;

    // Link Rack → Site so the physical hierarchy is traversable without Device as intermediary.
    if !site.is_empty() {
        let site_id = format!("site:{site}");
        let mut rack_site = conn
            .prepare(
                "MATCH (r:Rack {id: $rack_id}), (s:Site {id: $site_id}) \
                 MERGE (r)-[:RACK_IN_SITE]->(s)",
            )
            .context("prepare RACK_IN_SITE merge")?;
        if let Err(e) = conn.execute(
            &mut rack_site,
            vec![
                ("rack_id", Value::String(rack_id)),
                ("site_id", Value::String(site_id)),
            ],
        ) {
            tracing::debug!(error = %e, site, "RACK_IN_SITE edge skipped (site not yet written)");
        }
    }
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
            ("id", Value::String(loc_id.clone())),
            ("name", Value::String(location_name.to_string())),
            ("kind", Value::String(kind.to_string())),
            ("site_id", Value::String(site_id.to_string())),
            ("src", Value::String(source.to_string())),
            ("ts", now.clone()),
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
        .prepare("MATCH (d:Device {address: $addr})-[r:IN_LOCATION]->(:Location) DELETE r")
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
            ("lid", Value::String(loc_id)),
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

/// DS-1 T5/T6: Extended flow fields for DDoS signal extraction.
#[derive(Default)]
pub struct AppFlowDdosFields {
    pub tcp_flags: u8,
    pub tcp_flags_pattern: String,
    pub src_as: u32,
    pub dst_as: u32,
    pub input_snmp: u32,
    pub flow_direction: u8,
    pub amplification_vector: String,
    pub icmp_type: u8,
    pub icmp_code: u8,
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
    upsert_app_flow_ext(
        conn,
        id,
        exporter_address,
        src_address,
        dst_address,
        dst_port,
        protocol,
        bytes_per_sec,
        packets_per_sec,
        now_ns,
        &AppFlowDdosFields::default(),
    )
}

pub fn upsert_app_flow_ext(
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
    ddos: &AppFlowDdosFields,
) -> Result<()> {
    let now = ts(now_ns);
    let mut stmt = conn
        .prepare(
            "MERGE (f:AppFlow {id: $id}) \
             ON CREATE SET f.exporter_address = $exp, f.src_address = $src, f.dst_address = $dst, \
               f.dst_port = $port, f.protocol = $proto, f.bytes_per_sec = $bps, \
               f.packets_per_sec = $pps, f.updated_at = $ts, \
               f.tcp_flags = $tcpf, f.tcp_flags_pattern = $tcpfp, \
               f.src_as = $srcas, f.dst_as = $dstas, \
               f.input_snmp = $isnmp, f.flow_direction = $fdir, \
               f.amplification_vector = $ampv, f.icmp_type = $icmpt, f.icmp_code = $icmpc \
             ON MATCH SET f.exporter_address = $exp, f.bytes_per_sec = $bps, \
               f.packets_per_sec = $pps, f.updated_at = $ts, \
               f.tcp_flags = $tcpf, f.tcp_flags_pattern = $tcpfp, \
               f.amplification_vector = $ampv, f.icmp_type = $icmpt, f.icmp_code = $icmpc",
        )
        .context("prepare AppFlow upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("exp", Value::String(exporter_address.to_string())),
            ("src", Value::String(src_address.to_string())),
            ("dst", Value::String(dst_address.to_string())),
            ("port", Value::Int64(dst_port)),
            ("proto", Value::String(protocol.to_string())),
            ("bps", Value::Double(bytes_per_sec)),
            ("pps", Value::Double(packets_per_sec)),
            ("ts", now),
            ("tcpf", Value::Int64(ddos.tcp_flags as i64)),
            ("tcpfp", Value::String(ddos.tcp_flags_pattern.clone())),
            ("srcas", Value::Int64(ddos.src_as as i64)),
            ("dstas", Value::Int64(ddos.dst_as as i64)),
            ("isnmp", Value::Int64(ddos.input_snmp as i64)),
            ("fdir", Value::Int64(ddos.flow_direction as i64)),
            ("ampv", Value::String(ddos.amplification_vector.clone())),
            ("icmpt", Value::Int64(ddos.icmp_type as i64)),
            ("icmpc", Value::Int64(ddos.icmp_code as i64)),
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
            ("id", Value::String(ip.to_string())),
            ("ip", Value::String(ip.to_string())),
            ("kind", Value::String(effective_kind.to_string())),
            ("hostname", Value::String(hostname.to_string())),
            ("mac", Value::String(mac.to_string())),
            ("vendor", Value::String(vendor.to_string())),
            ("rack_id", Value::String(rack_id.to_string())),
            ("site_id", Value::String(site_id.to_string())),
            ("src", Value::String(source.to_string())),
            ("ts", now),
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

/// Deterministic outcome of resolving a peer address to a Device.
///
/// A BGP neighbor observation and a resolved device-to-device
/// relationship are different entities: this enum is the contract
/// between them. `Unresolved`, `Ambiguous` and `Conflicted` are all
/// explicit non-results — callers must never substitute an arbitrary
/// candidate for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerResolution {
    /// Exactly one device claims the address, consistently across all
    /// evidence tiers that have an opinion.
    Resolved(String),
    /// No device claims the address.
    Unresolved,
    /// Multiple distinct devices claim the address within the alias or
    /// identity tiers (no authoritative primary-address claim).
    Ambiguous,
    /// The primary `Device.address` tier and the alias/identity tiers
    /// disagree about ownership.
    Conflicted { primary: String, alias: String },
    /// One or more required evidence tiers could not be examined (query
    /// preparation/execution failure). This is NOT a successful "no
    /// results" lookup — resolution is unavailable and callers must not
    /// treat any candidate as authoritative.
    Unavailable { detail: String },
}

impl PeerResolution {
    /// The resolved device address, or `None` for every non-result.
    pub fn device(&self) -> Option<&str> {
        match self {
            PeerResolution::Resolved(device) => Some(device),
            _ => None,
        }
    }

    /// Stable machine-readable status for API/UI consumers.
    pub fn status(&self) -> &'static str {
        match self {
            PeerResolution::Resolved(_) => "resolved",
            PeerResolution::Unresolved => "unresolved",
            PeerResolution::Ambiguous => "ambiguous",
            PeerResolution::Conflicted { .. } => "conflict",
            PeerResolution::Unavailable { .. } => "unavailable",
        }
    }

    /// Human/machine-readable explanation for non-resolved outcomes.
    /// Empty for `Resolved`.
    pub fn reason(&self) -> String {
        match self {
            PeerResolution::Resolved(_) => String::new(),
            PeerResolution::Unresolved => "no_device_claims_address".to_string(),
            PeerResolution::Ambiguous => "multiple_devices_claim_address".to_string(),
            PeerResolution::Conflicted { primary, alias } => {
                format!("primary_address_owner={primary} alias_owner={alias}")
            }
            PeerResolution::Unavailable { detail } => detail.clone(),
        }
    }
}

/// Resolve a peer IP address to the Device that owns it, collecting ALL
/// evidence before deciding. No `LIMIT 1` arbitrary picks, no prefix
/// matches, no provisional flow/syslog/SNMP/OTLP claims.
///
/// Evidence tiers (all must agree for `Resolved`):
///   1. Exact `Device.address` match — the primary, authoritative key.
///   2. `DeviceAddress.ip` → `KNOWN_ADDRESS_OF` — registered aliases
///      (extra_ips, loopbacks, protocol-observed addresses).
///   3. `EntityIdentity.mgmt_ip` → `HAS_IDENTITY` — identity evidence.
///
/// Multiple pieces of evidence pointing at the SAME device deduplicate
/// to `Resolved`. Distinct claimants produce `Ambiguous` (within a
/// tier) or `Conflicted` (primary vs alias/identity disagreement).
pub fn resolve_peer_typed(conn: &Connection<'_>, peer_ip: &str) -> PeerResolution {
    if peer_ip.is_empty() {
        return PeerResolution::Unresolved;
    }
    let bare = crate::registry::strip_port(peer_ip);

    // Each evidence tier is a required lookup. A failed prepare/execute is
    // NOT the same as a successful lookup with zero rows: it means the
    // evidence could not be examined, so the resolution is Unavailable
    // rather than Resolved on partial evidence (fail closed).
    let tier = |label: &str,
                cypher: &str|
     -> std::result::Result<Vec<String>, String> {
        let mut stmt = conn
            .prepare(cypher)
            .map_err(|e| format!("{label}: prepare failed: {e}"))?;
        let mut rows = conn
            .execute(&mut stmt, vec![("ip", Value::String(bare.to_string()))])
            .map_err(|e| format!("{label}: execute failed: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next() {
            if let Value::String(addr) = &row[0] {
                if !addr.is_empty() && !out.contains(addr) {
                    out.push(addr.clone());
                }
            }
        }
        Ok(out)
    };

    let primary = tier(
        "primary_address",
        "MATCH (d:Device {address: $ip}) RETURN DISTINCT d.address",
    );
    let aliases = tier(
        "alias_address",
        "MATCH (a:DeviceAddress {ip: $ip})-[:KNOWN_ADDRESS_OF]->(d:Device) \
         RETURN DISTINCT d.address",
    );
    let identities = tier(
        "entity_identity",
        "MATCH (e:EntityIdentity {mgmt_ip: $ip})<-[:HAS_IDENTITY]-(d:Device) \
         RETURN DISTINCT d.address",
    );

    let (primary, aliases, identities) = match (primary, aliases, identities) {
        (Ok(p), Ok(a), Ok(i)) => (p, a, i),
        (p, a, i) => {
            let mut failures: Vec<String> = Vec::new();
            for (label, res) in [
                ("primary_address", &p),
                ("alias_address", &a),
                ("entity_identity", &i),
            ] {
                if let Err(e) = res {
                    failures.push(format!("{label}: {e}"));
                }
            }
            metrics::counter!("bonsai_peer_resolution_unavailable_total").increment(1);
            return PeerResolution::Unavailable {
                detail: format!("evidence_lookup_failed: {}", failures.join("; ")),
            };
        }
    };

    // Union of all claimants, deduplicated.
    let mut all: Vec<String> = primary.clone();
    for addr in aliases.iter().chain(identities.iter()) {
        if !all.contains(addr) {
            all.push(addr.clone());
        }
    }

    match all.len() {
        0 => PeerResolution::Unresolved,
        1 => PeerResolution::Resolved(all[0].clone()),
        _ => {
            // Primary address is the strongest claim. If it exists and
            // any alias/identity evidence names a DIFFERENT device, the
            // evidence contradicts — explicit conflict, never a guess.
            if let Some(primary_addr) = primary.first() {
                if let Some(other) = all.iter().find(|a| *a != primary_addr) {
                    return PeerResolution::Conflicted {
                        primary: primary_addr.clone(),
                        alias: other.clone(),
                    };
                }
            }
            PeerResolution::Ambiguous
        }
    }
}

/// Attempt to resolve a peer IP address (loopback, interface IP, mgmt IP, etc.) to a
/// known Device's primary address (bare IP PK).
///
/// Thin wrapper over [`resolve_peer_typed`]: returns `Some(device)` only
/// for a uniquely, consistently resolved device. Ambiguous, conflicting
/// and unknown peers all return `None` — callers must not substitute an
/// arbitrary candidate.
///
/// Returns `Some(device_address)` on success, `None` when peer is unknown
/// or resolution is not unique. All failures are silent (best-effort) so
/// callers should treat None as "not yet known".
pub fn resolve_peer_to_device(conn: &Connection<'_>, peer_ip: &str) -> Option<String> {
    resolve_peer_typed(conn, peer_ip).device().map(str::to_string)
}

/// Identity of ONE BGP session observation as projected onto a
/// `BGP_SESSION_WITH` Device→Device edge.
///
/// `session_id` is the `BgpNeighbor` node id — the same identifier the
/// session evidence uses — so distinct sessions that share a peer
/// address (different BMP peer-type/route-distinguisher, different gNMI
/// network-instances) never collapse onto one relationship.
pub(crate) struct BgpSessionEdgeKey<'a> {
    pub src_device: &'a str,
    /// Protocol provenance: "gnmi" or "bmp".
    pub source: &'a str,
    pub peer_address: &'a str,
    /// `BgpNeighbor.id` of the session node this edge projects.
    pub session_id: &'a str,
    /// BMP peer-type; -1 when the source does not provide one.
    pub peer_type: i64,
    /// BMP route distinguisher; "" when not provided.
    pub route_distinguisher: String,
    /// gNMI network-instance context; "" when not provided.
    pub network_instance: String,
}

/// Transactionally reconcile the `BGP_SESSION_WITH` Device→Device edge
/// for ONE session observation, keyed by (src device, source protocol,
/// peer address, session id).
///
/// * `resolved_device = Some(dst)` — the peer address resolved uniquely:
///   replace any edge for THIS session with a single edge to `dst`
///   carrying the observed session state.
/// * `resolved_device = None` — unresolved/ambiguous/conflicting/
///   unavailable: every edge for THIS session is removed. A session
///   observation that cannot be resolved must never leave a previous
///   resolution presenting itself as current. Edges belonging to OTHER
///   sessions (different `session_id`) are untouched.
///
/// The caller owns the surrounding transaction: inside `write_batch`
/// this participates in the batch transaction; inside `write` the
/// single-write transaction; elsewhere statements auto-commit. lbug
/// rejects nested BEGIN TRANSACTION, so this function must never open
/// its own.
///
/// Edges predating `session_id` (`NULL`/`''`) are NOT touched here —
/// they are handled once by `migrate_bgp_session_edges` at store open,
/// which re-keys them only when exactly one session node can claim
/// them and invalidates the rest.
pub(crate) fn reconcile_bgp_session_edge(
    conn: &Connection<'_>,
    key: &BgpSessionEdgeKey<'_>,
    resolved_device: Option<&str>,
    session_state: &str,
    peer_as: i64,
) -> Result<()> {
    let mut clear = conn
        .prepare(
            "MATCH (a:Device {address: $src})-[r:BGP_SESSION_WITH]->() \
             WHERE r.source = $source AND r.session_id = $sid \
             DELETE r",
        )
        .context("prepare BGP_SESSION_WITH reconcile clear")?;
    conn.execute(
        &mut clear,
        vec![
            ("src", Value::String(key.src_device.to_string())),
            ("source", Value::String(key.source.to_string())),
            ("sid", Value::String(key.session_id.to_string())),
        ],
    )
    .context("execute BGP_SESSION_WITH reconcile clear")?;

    if let Some(dst) = resolved_device {
        let mut link = conn
            .prepare(
                "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                 MERGE (a)-[r:BGP_SESSION_WITH {source: $source, peer_address: $peer, session_id: $sid}]->(b) \
                 SET r.session_state = $state, r.peer_as = $peer_as, \
                     r.peer_type = $pt, r.route_distinguisher = $rd, \
                     r.network_instance = $ni",
            )
            .context("prepare BGP_SESSION_WITH reconcile merge")?;
        conn.execute(
            &mut link,
            vec![
                ("src", Value::String(key.src_device.to_string())),
                ("dst", Value::String(dst.to_string())),
                ("source", Value::String(key.source.to_string())),
                ("peer", Value::String(key.peer_address.to_string())),
                ("sid", Value::String(key.session_id.to_string())),
                ("state", Value::String(session_state.to_string())),
                ("peer_as", Value::Int64(peer_as)),
                ("pt", Value::Int64(key.peer_type)),
                ("rd", Value::String(key.route_distinguisher.clone())),
                ("ni", Value::String(key.network_instance.clone())),
            ],
        )
        .context("execute BGP_SESSION_WITH reconcile merge")?;
    }
    Ok(())
}

/// Re-resolve every BGP session that names `peer_ip` and reconcile its
/// stored `BGP_SESSION_WITH` edge against CURRENT ownership evidence.
///
/// Called when ownership evidence mutates (KNOWN_ADDRESS_OF links,
/// EntityIdentity mgmt_ip claims) so a stored relationship cannot keep
/// presenting stale ownership as current while no new BGP observation
/// arrives. The lookup is scoped to `BgpNeighbor` rows for this one
/// peer address — never a whole-graph scan.
pub(crate) fn reconcile_bgp_session_edges_for_peer(
    conn: &Connection<'_>,
    peer_ip: &str,
) -> Result<()> {
    if peer_ip.is_empty() {
        return Ok(());
    }
    // Read all session rows first: lbug does not allow an open read
    // cursor and writes on the same connection simultaneously.
    let mut stmt = conn
        .prepare(
            "MATCH (n:BgpNeighbor) WHERE n.peer_address = $ip \
             RETURN n.id, n.device_address, n.peer_address, n.source, \
                    n.session_state, n.peer_as, n.peer_type, \
                    n.route_distinguisher, n.network_instance",
        )
        .context("prepare BGP peer session scan")?;
    let rows: Vec<Vec<Value>> = conn
        .execute(&mut stmt, vec![("ip", Value::String(peer_ip.to_string()))])
        .context("execute BGP peer session scan")?
        .collect();
    if rows.is_empty() {
        return Ok(());
    }

    let resolution = resolve_peer_typed(conn, peer_ip);
    for row in rows {
        let session_id = read_str(&row[0]);
        let src = read_str(&row[1]);
        let peer = read_str(&row[2]);
        let source = {
            let s = read_str(&row[3]);
            if s.is_empty() {
                "gnmi".to_string()
            } else {
                s
            }
        };
        let state = read_str(&row[4]);
        let peer_as = match &row[5] {
            Value::Int64(n) => *n,
            Value::Int32(n) => *n as i64,
            _ => 0,
        };
        let peer_type = match &row[6] {
            Value::Int64(n) => *n,
            Value::Int32(n) => *n as i64,
            _ => -1,
        };
        let rd = read_str(&row[7]);
        let ni = read_str(&row[8]);
        reconcile_bgp_session_edge(
            conn,
            &BgpSessionEdgeKey {
                src_device: &src,
                source: &source,
                peer_address: &peer,
                session_id: &session_id,
                peer_type,
                route_distinguisher: rd,
                network_instance: ni,
            },
            resolution.device(),
            &state,
            peer_as,
        )?;
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

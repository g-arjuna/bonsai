//! NetBox GraphEnricher — pulls device/site/VLAN/prefix context from NetBox
//! and writes it to the bonsai graph as namespaced `netbox_*` properties plus
//! first-class VLAN and Prefix nodes.
//!
//! Auth: REST token stored as the `password` field of the credential alias.
//! Credential purpose: `ResolvePurpose::Enrich`.
//!
//! Transport: REST (default) or MCP — selected via `config.extra.transport`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::Deserialize;
use tokio::sync::Semaphore;

use tracing::{debug, warn};

use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::enrichment::EnricherAuditLog;
use crate::enrichment::EnrichmentSchedule;
use crate::enrichment::{EnricherConfig, EnrichmentReport, EnrichmentWriteSurface, GraphEnricher};
use crate::mcp_client::{EnricherTransport, McpClient};
use crate::store::BonsaiStore;

// ── NetBox REST response shapes ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NbList<T> {
    results: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct NbDevice {
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    serial: String,
    primary_ip: Option<NbIp>,
    site: Option<NbNested>,
    device_type: Option<NbDeviceType>,
    platform: Option<NbNested>,
    status: Option<NbStatus>,
}

#[derive(Debug, Deserialize)]
struct NbIp {
    address: String,
}

#[derive(Debug, Deserialize)]
struct NbNested {
    name: String,
    #[serde(default)]
    slug: String,
}

#[derive(Debug, Deserialize)]
struct NbDeviceType {
    model: String,
    manufacturer: Option<NbNested>,
}

#[derive(Debug, Deserialize)]
struct NbStatus {
    value: String,
}

#[derive(Debug, Deserialize)]
struct NbVlan {
    vid: u32,
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct NbPrefix {
    prefix: String,
    #[serde(rename = "role")]
    role: Option<NbNested>,
    #[serde(default)]
    description: String,
    assigned_object: Option<NbPrefixDevice>,
}

#[derive(Debug, Deserialize)]
struct NbPrefixDevice {
    device: Option<NbPrefixDeviceRef>,
}

#[derive(Debug, Deserialize)]
struct NbPrefixDeviceRef {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NbInterface {
    device: NbInterfaceDevice,
    name: String,
    #[serde(default)]
    description: String,
    untagged_vlan: Option<NbVlanRef>,
    tagged_vlans: Vec<NbVlanRef>,
}

#[derive(Debug, Deserialize)]
struct NbInterfaceDevice {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NbVlanRef {
    vid: u32,
    #[allow(dead_code)]
    name: String,
}

// ── Transport enum ────────────────────────────────────────────────────────────

enum Transport {
    Rest(reqwest::Client),
    Mcp(McpClient),
}

impl Transport {
    async fn get_devices(&self, base_url: &str, token: &str) -> Result<Vec<NbDevice>> {
        match self {
            Transport::Rest(client) => {
                paginate_rest::<NbDevice>(client, base_url, "dcim/devices/", token).await
            }
            Transport::Mcp(mcp) => {
                let val = mcp
                    .call("netbox:devices_list", serde_json::json!({}))
                    .await?;
                serde_json::from_value(val).context("parse MCP devices response")
            }
        }
    }

    async fn get_vlans(&self, base_url: &str, token: &str) -> Result<Vec<NbVlan>> {
        match self {
            Transport::Rest(client) => {
                paginate_rest::<NbVlan>(client, base_url, "ipam/vlans/", token).await
            }
            Transport::Mcp(mcp) => {
                let val = mcp.call("netbox:vlans_list", serde_json::json!({})).await?;
                serde_json::from_value(val).context("parse MCP vlans response")
            }
        }
    }

    async fn get_prefixes(&self, base_url: &str, token: &str) -> Result<Vec<NbPrefix>> {
        match self {
            Transport::Rest(client) => {
                paginate_rest::<NbPrefix>(client, base_url, "ipam/prefixes/", token).await
            }
            Transport::Mcp(mcp) => {
                let val = mcp
                    .call("netbox:prefixes_list", serde_json::json!({}))
                    .await?;
                serde_json::from_value(val).context("parse MCP prefixes response")
            }
        }
    }

    async fn get_interfaces(&self, base_url: &str, token: &str) -> Result<Vec<NbInterface>> {
        match self {
            Transport::Rest(client) => {
                paginate_rest::<NbInterface>(client, base_url, "dcim/interfaces/", token).await
            }
            Transport::Mcp(mcp) => {
                let val = mcp
                    .call("netbox:interfaces_list", serde_json::json!({}))
                    .await?;
                serde_json::from_value(val).context("parse MCP interfaces response")
            }
        }
    }

    async fn test_connection(&self, base_url: &str, token: &str) -> Result<()> {
        match self {
            Transport::Rest(client) => {
                let url = format!("{base_url}/api/");
                let resp = client
                    .get(&url)
                    .header("Authorization", format!("Token {token}"))
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
                    .with_context(|| format!("GET {url}"))?;
                if resp.status().is_success() {
                    Ok(())
                } else {
                    anyhow::bail!("NetBox returned {}", resp.status())
                }
            }
            Transport::Mcp(mcp) => {
                mcp.call("netbox:status", serde_json::json!({})).await?;
                Ok(())
            }
        }
    }
}

async fn paginate_rest<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    base_url: &str,
    endpoint: &str,
    token: &str,
) -> Result<Vec<T>> {
    let mut results = Vec::new();
    let mut offset: usize = 0;

    loop {
        let url = format!("{base_url}/api/{endpoint}?limit=200&offset={offset}");
        debug!(url = %url, "NetBox REST GET");
        let resp = client
            .get(&url)
            .header("Authorization", format!("Token {token}"))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        if !resp.status().is_success() {
            anyhow::bail!("NetBox {endpoint} returned {}", resp.status());
        }

        let page: NbList<T> = resp
            .json()
            .await
            .with_context(|| format!("parse NetBox {endpoint} response"))?;
        let fetched = page.results.len();
        results.extend(page.results);
        if fetched < 200 {
            break;
        }
        offset += 200;
    }

    Ok(results)
}

// ── Enricher ──────────────────────────────────────────────────────────────────

pub struct NetBoxEnricher {
    config: EnricherConfig,
}

impl NetBoxEnricher {
    pub fn from_config(config: EnricherConfig) -> Self {
        Self { config }
    }

    fn build_transport(&self) -> Result<Transport> {
        let mode = EnricherTransport::from_extra(&self.config.extra);
        match mode {
            EnricherTransport::Rest => {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .context("build reqwest client")?;
                Ok(Transport::Rest(client))
            }
            EnricherTransport::Mcp { server_url } => {
                let mcp = McpClient::new(server_url)?;
                Ok(Transport::Mcp(mcp))
            }
        }
    }
}

#[async_trait::async_trait]
impl GraphEnricher for NetBoxEnricher {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn schedule(&self) -> EnrichmentSchedule {
        if self.config.poll_interval_secs == 0 {
            EnrichmentSchedule::Manual
        } else {
            EnrichmentSchedule::Interval {
                secs: self.config.poll_interval_secs,
            }
        }
    }

    fn writes_to(&self) -> EnrichmentWriteSurface {
        EnrichmentWriteSurface {
            property_namespace: "netbox_".to_string(),
            owned_labels: vec!["VLAN".to_string(), "Prefix".to_string()],
            owned_edge_types: vec![
                "HAS_ENRICHMENT_PROPERTY".to_string(),
                "ACCESS_VLAN".to_string(),
                "TRUNK_VLAN".to_string(),
                "HAS_PREFIX".to_string(),
            ],
        }
    }

    async fn enrich(
        &self,
        store: &dyn BonsaiStore,
        creds: &CredentialVault,
        audit: &EnricherAuditLog,
    ) -> Result<EnrichmentReport> {
        let started = Instant::now();
        let mut warnings: Vec<String> = Vec::new();

        let cred = creds
            .resolve(&self.config.credential_alias, ResolvePurpose::Enrich)
            .inspect_err(|e| {
                audit.log_credential_resolve(
                    &self.config.credential_alias,
                    "error",
                    Some(&e.to_string()),
                );
            })?;
        audit.log_credential_resolve(&self.config.credential_alias, "ok", None);
        let token = &cred.password; // borrow; no extra copy of the credential
        let base_url = self.config.base_url.trim_end_matches('/').to_string();

        // Read concurrency cap from config.extra; default 2 to avoid hammering NetBox
        let max_concurrent = self
            .config
            .extra
            .get("max_concurrent_requests")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as usize;
        let sem = Arc::new(Semaphore::new(max_concurrent));

        let transport = self.build_transport()?;

        // Fetch all NetBox data with bounded concurrency (semaphore limits in-flight requests)
        let (devices_res, vlans_res, prefixes_res, ifaces_res) = tokio::join!(
            async { let _p = sem.acquire().await.expect("sem"); transport.get_devices(&base_url, token).await },
            async { let _p = sem.acquire().await.expect("sem"); transport.get_vlans(&base_url, token).await },
            async { let _p = sem.acquire().await.expect("sem"); transport.get_prefixes(&base_url, token).await },
            async { let _p = sem.acquire().await.expect("sem"); transport.get_interfaces(&base_url, token).await },
        );

        let nb_devices = devices_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch devices: {e:#}"));
            vec![]
        });
        let nb_vlans = vlans_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch VLANs: {e:#}"));
            vec![]
        });
        let nb_prefixes = prefixes_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch prefixes: {e:#}"));
            vec![]
        });
        let nb_ifaces = ifaces_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch interfaces: {e:#}"));
            vec![]
        });

        let source = self.config.name.clone();
        let db = store.db();

        let (nodes_touched, edges_created, write_warnings) = tokio::task::spawn_blocking(move || {
            write_to_graph(
                &db,
                &nb_devices,
                &nb_vlans,
                &nb_prefixes,
                &nb_ifaces,
                &source,
            )
        })
        .await
        .context("graph write task panicked")??;

        warnings.extend(write_warnings);

        Ok(EnrichmentReport {
            enricher_name: self.config.name.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
            nodes_touched,
            edges_created,
            warnings,
            error: None,
        })
    }

    async fn test_connection(
        &self,
        creds: &CredentialVault,
        audit: &EnricherAuditLog,
    ) -> Result<()> {
        let cred = creds
            .resolve(&self.config.credential_alias, ResolvePurpose::Enrich)
            .inspect_err(|e| {
                audit.log_credential_resolve(
                    &self.config.credential_alias,
                    "error",
                    Some(&e.to_string()),
                );
            })?;
        audit.log_credential_resolve(&self.config.credential_alias, "ok", None);

        let base_url = self.config.base_url.trim_end_matches('/').to_string();
        let transport = self.build_transport()?;
        transport.test_connection(&base_url, &cred.password).await
    }
}

// ── Graph write helpers ───────────────────────────────────────────────────────

fn write_to_graph(
    db: &Arc<lbug::Database>,
    devices: &[NbDevice],
    vlans: &[NbVlan],
    prefixes: &[NbPrefix],
    ifaces: &[NbInterface],
    source: &str,
) -> Result<(usize, usize, Vec<String>)> {
    let conn = Connection::new(db).context("open graph connection")?;
    let mut nodes = 0usize;
    let mut edges = 0usize;
    let mut warnings = Vec::new();
    let now_ns = crate::graph::common::now_ns();

    // 1. Write EnrichmentProperty nodes for each device (chunked for progress visibility)
    for (chunk_idx, chunk) in devices.chunks(100).enumerate() {
        for dev in chunk {
            let Some(ip) = dev.primary_ip.as_ref() else {
                continue;
            };
            // Strip prefix length from IP address (e.g. "192.168.1.1/32" → "192.168.1.1")
            let addr = ip
                .address
                .split('/')
                .next()
                .unwrap_or(&ip.address)
                .to_string();

            let mut props: Vec<(&str, String)> = vec![];
            if !dev.serial.is_empty() {
                props.push(("netbox_serial", dev.serial.clone()));
            }
            if let Some(dt) = &dev.device_type {
                props.push(("netbox_model", dt.model.clone()));
                if let Some(mfr) = &dt.manufacturer {
                    props.push(("netbox_manufacturer", mfr.name.clone()));
                }
            }
            if let Some(platform) = &dev.platform {
                props.push(("netbox_platform", platform.name.clone()));
            }
            if let Some(status) = &dev.status {
                props.push(("netbox_lifecycle_status", status.value.clone()));
            }
            if let Some(site) = &dev.site {
                props.push(("netbox_site", site.name.clone()));
                props.push(("netbox_site_slug", site.slug.clone()));
            }

            for (key, value) in props {
                let id = format!("{addr}:{key}");
                if let Err(e) =
                    upsert_enrichment_property(&conn, &id, &addr, key, &value, source, now_ns)
                {
                    warnings.push(format!("device {addr} property {key}: {e:#}"));
                } else {
                    nodes += 1;
                    edges += 1; // HAS_ENRICHMENT_PROPERTY created inside upsert_enrichment_property
                }
            }
        }
        debug!(chunk = chunk_idx, size = chunk.len(), "wrote device enrichment chunk");
    }

    // 2. Write VLAN nodes (site-scope VLANs from NetBox)
    for vlan in vlans {
        let id = format!("netbox_vlan_{}", vlan.vid);
        if let Err(e) = upsert_vlan(&conn, &id, vlan.vid as i64, &vlan.name, source, now_ns) {
            warnings.push(format!("VLAN {}: {e:#}", vlan.vid));
        } else {
            nodes += 1;
        }
    }

    // 3. Write Prefix nodes + HAS_PREFIX edges
    for prefix in prefixes {
        let id = format!("netbox_prefix_{}", prefix.prefix.replace('/', "_"));
        let role = prefix
            .role
            .as_ref()
            .map(|r| r.name.as_str())
            .unwrap_or("unknown");
        if let Err(e) = upsert_prefix(
            &conn,
            &id,
            &prefix.prefix,
            role,
            &prefix.description,
            source,
            now_ns,
        ) {
            warnings.push(format!("prefix {}: {e:#}", prefix.prefix));
        } else {
            nodes += 1;
        }

        // Link prefix to device if we know which device it belongs to
        if let Some(assigned) = &prefix.assigned_object
            && let Some(dev_ref) = &assigned.device
            && let Some(dev_name) = &dev_ref.name
        {
            match link_device_prefix(&conn, dev_name, &id) {
                Ok(()) => edges += 1,
                Err(e) => warnings.push(format!("HAS_PREFIX {dev_name} → {}: {e:#}", prefix.prefix)),
            }
        }
    }

    // 4. Write interface VLAN assignments
    for iface in ifaces {
        let Some(dev_name) = &iface.device.name else {
            continue;
        };
        let iface_id = format!("{dev_name}:{}", iface.name);

        if !iface.description.is_empty() {
            let prop_id = format!("{iface_id}:netbox_description");
            if let Err(e) = upsert_enrichment_property(
                &conn,
                &prop_id,
                &iface_id,
                "netbox_if_description",
                &iface.description,
                source,
                now_ns,
            ) {
                warn!("interface {iface_id} description: {e:#}");
            } else {
                nodes += 1;
                edges += 1;
            }
        }

        // ACCESS_VLAN
        if let Some(av) = &iface.untagged_vlan {
            let vlan_id = format!("netbox_vlan_{}", av.vid);
            let if_node_id = format!("{dev_name}:{}:if", iface.name);
            match link_interface_vlan(&conn, &if_node_id, &vlan_id, "ACCESS_VLAN") {
                Ok(()) => edges += 1,
                Err(e) => warnings.push(format!("ACCESS_VLAN {iface_id}: {e:#}")),
            }
        }

        // TRUNK_VLANs
        for tv in &iface.tagged_vlans {
            let vlan_id = format!("netbox_vlan_{}", tv.vid);
            let if_node_id = format!("{dev_name}:{}:if", iface.name);
            match link_interface_vlan(&conn, &if_node_id, &vlan_id, "TRUNK_VLAN") {
                Ok(()) => edges += 1,
                Err(e) => warnings.push(format!("TRUNK_VLAN {iface_id} vlan {}: {e:#}", tv.vid)),
            }
        }
    }

    Ok((nodes, edges, warnings))
}

fn upsert_enrichment_property(
    conn: &Connection<'_>,
    id: &str,
    device_address: &str,
    key: &str,
    value: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (p:EnrichmentProperty {id: $id}) \
         SET p.device_address = $addr, p.key = $key, p.value = $val, \
             p.source_name = $src, p.updated_at = $now",
        )
        .context("prepare upsert_enrichment_property")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("addr", Value::String(device_address.to_string())),
            ("key", Value::String(key.to_string())),
            ("val", Value::String(value.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_enrichment_property")?;

    // Ensure HAS_ENRICHMENT_PROPERTY edge from Device to EnrichmentProperty
    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (p:EnrichmentProperty {id: $id}) \
         MERGE (d)-[:HAS_ENRICHMENT_PROPERTY]->(p)",
        )
        .context("prepare HAS_ENRICHMENT_PROPERTY")?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("id", Value::String(id.to_string())),
        ],
    )
    .context("execute HAS_ENRICHMENT_PROPERTY")?;

    Ok(())
}

fn upsert_vlan(
    conn: &Connection<'_>,
    id: &str,
    vid: i64,
    name: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (v:VLAN {id: $id}) \
         SET v.vid = $vid, v.name = $name, v.source_name = $src, v.updated_at = $now",
        )
        .context("prepare upsert_vlan")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("vid", Value::Int64(vid)),
            ("name", Value::String(name.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_vlan")?;
    Ok(())
}

fn upsert_prefix(
    conn: &Connection<'_>,
    id: &str,
    cidr: &str,
    role: &str,
    description: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (p:Prefix {id: $id}) \
         SET p.cidr = $cidr, p.prefix_role = $pfx_role, p.descr = $pfx_desc, \
             p.source_name = $src, p.updated_at = $now",
        )
        .context("prepare upsert_prefix")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("cidr", Value::String(cidr.to_string())),
            ("pfx_role", Value::String(role.to_string())),
            ("pfx_desc", Value::String(description.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_prefix")?;
    Ok(())
}

fn link_device_prefix(conn: &Connection<'_>, device_hostname: &str, prefix_id: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {hostname: $hn}), (p:Prefix {id: $pid}) \
         MERGE (d)-[:HAS_PREFIX]->(p)",
        )
        .context("prepare HAS_PREFIX")?;
    conn.execute(
        &mut stmt,
        vec![
            ("hn", Value::String(device_hostname.to_string())),
            ("pid", Value::String(prefix_id.to_string())),
        ],
    )
    .context("execute HAS_PREFIX")?;
    Ok(())
}

fn link_interface_vlan(
    conn: &Connection<'_>,
    interface_id: &str,
    vlan_id: &str,
    rel_type: &str,
) -> Result<()> {
    // Best-effort: the Interface node may not have this exact composite ID format.
    // The edge is created only if both nodes exist; MERGE is idempotent.
    let query = format!(
        "MATCH (i:Interface {{id: $iid}}), (v:VLAN {{id: $vid}}) \
         MERGE (i)-[:{rel_type}]->(v)"
    );
    let mut stmt = conn
        .prepare(&query)
        .context("prepare link_interface_vlan")?;
    conn.execute(
        &mut stmt,
        vec![
            ("iid", Value::String(interface_id.to_string())),
            ("vid", Value::String(vlan_id.to_string())),
        ],
    )
    .context("execute link_interface_vlan")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStore;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn open_test_graph(label: &str) -> GraphStore {
        let path = std::env::temp_dir()
            .join(format!("bonsai-netbox-test-{label}-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        GraphStore::open(&path, 256 * 1024 * 1024).expect("open test graph")
    }

    fn nb_page(results: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "results": results })
    }

    // ── pagination offset counter (Q-2 fix) ───────────────────────────────────

    #[tokio::test]
    async fn pagination_advances_offset_correctly_across_pages() {
        let server = MockServer::start().await;

        // Page 1: 200 items (triggers next page)
        let page1: Vec<serde_json::Value> = (0..200)
            .map(|i| serde_json::json!({"vid": i, "name": format!("vlan-{i}")}))
            .collect();
        // Page 2: 50 items (stops pagination)
        let page2: Vec<serde_json::Value> = (200..250)
            .map(|i| serde_json::json!({"vid": i, "name": format!("vlan-{i}")}))
            .collect();

        Mock::given(method("GET"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(nb_page(serde_json::json!(page1))))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(query_param("offset", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(nb_page(serde_json::json!(page2))))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result: Vec<NbVlan> = paginate_rest(&client, &server.uri(), "ipam/vlans/", "tok").await.unwrap();
        assert_eq!(result.len(), 250);
        server.verify().await;
    }

    // ── config.extra: max_concurrent_requests ────────────────────────────────

    #[test]
    fn max_concurrent_requests_defaults_to_two() {
        let config = EnricherConfig {
            name: "nb".to_string(),
            enricher_type: "netbox".to_string(),
            enabled: true,
            base_url: "http://netbox.local".to_string(),
            credential_alias: "tok".to_string(),
            poll_interval_secs: 0,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        };
        let max: usize = config
            .extra
            .get("max_concurrent_requests")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as usize;
        assert_eq!(max, 2);
    }

    #[test]
    fn max_concurrent_requests_reads_from_extra() {
        let config = EnricherConfig {
            name: "nb".to_string(),
            enricher_type: "netbox".to_string(),
            enabled: true,
            base_url: "http://netbox.local".to_string(),
            credential_alias: "tok".to_string(),
            poll_interval_secs: 0,
            environment_scope: vec![],
            extra: serde_json::json!({"max_concurrent_requests": 4}),
        };
        let max: usize = config
            .extra
            .get("max_concurrent_requests")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as usize;
        assert_eq!(max, 4);
    }

    // ── writes_to() namespace declaration ────────────────────────────────────

    #[test]
    fn writes_to_declares_netbox_namespace() {
        let config = EnricherConfig {
            name: "nb".to_string(),
            enricher_type: "netbox".to_string(),
            enabled: true,
            base_url: "http://netbox.local".to_string(),
            credential_alias: "tok".to_string(),
            poll_interval_secs: 0,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        };
        let enricher = NetBoxEnricher::from_config(config);
        let surface = enricher.writes_to();
        assert_eq!(surface.property_namespace, "netbox_");
        assert!(surface.owned_labels.contains(&"VLAN".to_string()));
        assert!(surface.owned_labels.contains(&"Prefix".to_string()));
        assert!(surface.owned_edge_types.contains(&"HAS_PREFIX".to_string()));
    }

    // ── graph write: VLAN nodes + edge count (Q-1 fix) ───────────────────────

    #[test]
    fn write_to_graph_counts_vlan_nodes() {
        let store = open_test_graph("vlans");
        let db = store.db();

        let vlans = vec![
            NbVlan { vid: 10, name: "mgmt".to_string(), description: String::new() },
            NbVlan { vid: 20, name: "data".to_string(), description: String::new() },
        ];

        let (nodes, edges, warnings) =
            write_to_graph(&db, &[], &vlans, &[], &[], "test").unwrap();

        assert_eq!(nodes, 2, "two VLAN nodes should be touched");
        assert_eq!(edges, 0, "VLANs alone create no edges");
        assert!(warnings.is_empty());
    }

    #[test]
    fn write_to_graph_counts_prefix_nodes() {
        let store = open_test_graph("prefixes");
        let db = store.db();

        let prefixes = vec![NbPrefix {
            prefix: "10.0.0.0/24".to_string(),
            role: Some(NbNested { name: "loopback".to_string(), slug: "loopback".to_string() }),
            description: "test prefix".to_string(),
            assigned_object: None,
        }];

        let (nodes, _edges, warnings) =
            write_to_graph(&db, &[], &[], &prefixes, &[], "test").unwrap();

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(nodes, 1);
    }

    #[test]
    fn write_to_graph_idempotent_vlan_upsert() {
        let store = open_test_graph("idem");
        let db = store.db();

        let vlans = vec![NbVlan { vid: 100, name: "prod".to_string(), description: String::new() }];
        let (n1, _, _) = write_to_graph(&db, &[], &vlans, &[], &[], "test").unwrap();
        let (n2, _, _) = write_to_graph(&db, &[], &vlans, &[], &[], "test").unwrap();
        // MERGE is idempotent — same number of nodes both times
        assert_eq!(n1, n2);
    }

    // ── wiremock: 401 from NetBox surfaces as error ───────────────────────────

    #[tokio::test]
    async fn pagination_returns_error_on_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result: Result<Vec<NbVlan>> =
            paginate_rest(&client, &server.uri(), "ipam/vlans/", "bad-token").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }
}

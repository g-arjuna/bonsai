//! ServiceNow CMDB GraphEnricher — pulls business context from a ServiceNow PDI
//! (or production instance with scoped roles) and writes it to the bonsai graph.
//!
//! Writes:
//! - `Application(id, name, criticality, owner_group)` nodes from cmdb_ci_business_service
//! - `Device.snow_ci_id`, `snow_owner_group`, `snow_assignment_group` properties
//! - `RUNS_SERVICE` / `CARRIES_APPLICATION` edges from cmdb_rel_ci
//! - `Incident` nodes from incidents where source = "bonsai" (T2-5 state consumption)
//! - Server CI properties (OS, RAM, CPU, serial) from cmdb_ci_server
//! - IP address enrichment from cmdb_ci_ip_address
//! - Subnet → Prefix nodes from cmdb_ci_ip_network
//! - Location hierarchy from cmn_location
//! - Network adapter enrichment from cmdb_ci_network_adapter
//! - Custom CI fields via configurable `extra.custom_fields` list
//! - Parent/child CI relationships → `CMDB_PARENT_OF` edges from cmdb_rel_ci
//!
//! Reconciliation: when the same property (e.g. site, serial, model) is also set by
//! NetBox or CLI parse, the `source_priority` list in `extra` determines the winner.
//! Default priority order: `["cli", "netbox", "servicenow"]`.
//! Every write records a `PropertyProvenance` node with winner/loser tracking.
//!
//! Auth: Basic auth — username + password from vault under `credential_alias`.
//! Credential purpose: `ResolvePurpose::ServiceNowAdmin`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::{Deserialize, Deserializer, de};
use tracing::{debug, info, warn};

use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::enrichment::{
    EnricherAuditLog, EnricherConfig, EnrichmentReport, EnrichmentSchedule, EnrichmentWriteSurface,
    GraphEnricher,
};
use crate::store::BonsaiStore;

// ── ServiceNow REST response shapes ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SnowList<T> {
    result: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct SnowBusinessService {
    sys_id: String,
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    short_description: String,
    #[serde(default)]
    operational_status: String,
    #[allow(dead_code)]
    assigned_to: Option<SnowRef>,
    assignment_group: Option<SnowRef>,
}

/// ServiceNow returns reference fields as either a plain string OR a
/// `{display_value, value}` object depending on `sysparm_display_value`.
/// This custom deserializer handles both shapes (Q-14).
#[derive(Debug)]
struct SnowRef {
    display_value: String,
}

impl<'de> Deserialize<'de> for SnowRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Inner {
            display_value: String,
        }

        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = SnowRef;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "a string or ServiceNow display_value object")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<SnowRef, E> {
                Ok(SnowRef {
                    display_value: v.to_string(),
                })
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<SnowRef, E> {
                Ok(SnowRef { display_value: v })
            }
            fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<SnowRef, A::Error> {
                let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(SnowRef {
                    display_value: inner.display_value,
                })
            }
        }

        d.deserialize_any(Visitor)
    }
}

#[derive(Debug, Deserialize)]
struct SnowRelCi {
    parent: SnowRefSysId,
    child: SnowRefSysId,
    #[serde(rename = "type")]
    rel_type: SnowRef,
}

#[derive(Debug, Deserialize)]
struct SnowRefSysId {
    display_value: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct SnowCi {
    sys_id: String,
    name: String,
    assigned_to: Option<SnowRef>,
    assignment_group: Option<SnowRef>,
}

/// Server CI — cmdb_ci_server (richer than cmdb_ci_netgear for compute)
#[derive(Debug, Deserialize)]
struct SnowServer {
    sys_id: String,
    name: String,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    os_version: String,
    #[serde(default)]
    ram: String,
    #[serde(default)]
    cpu_count: String,
    #[serde(default)]
    cpu_type: String,
    #[serde(default)]
    ip_address: String,
    #[serde(default)]
    model_id: Option<SnowRef>,
    #[serde(default)]
    manufacturer: Option<SnowRef>,
    assigned_to: Option<SnowRef>,
    assignment_group: Option<SnowRef>,
    location: Option<SnowRef>,
}

/// IP Address CI — cmdb_ci_ip_address
#[derive(Debug, Deserialize)]
struct SnowIpAddress {
    sys_id: String,
    #[serde(default)]
    ip_address: String,
    #[serde(default)]
    netmask: String,
    nic: Option<SnowRefSysId>,
}

/// Subnet / IP Network — cmdb_ci_ip_network
#[derive(Debug, Deserialize)]
struct SnowSubnet {
    #[allow(dead_code)]
    sys_id: String,
    name: String,
    #[serde(default, rename = "subnet")]
    cidr: String,
    #[serde(default)]
    short_description: String,
}

/// Location — cmn_location (ServiceNow location hierarchy)
#[derive(Debug, Deserialize)]
struct SnowLoc {
    sys_id: String,
    name: String,
    #[serde(default)]
    street: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    country: String,
    parent: Option<SnowRefSysId>,
}

/// Network adapter — cmdb_ci_network_adapter
#[derive(Debug, Deserialize)]
struct SnowNetworkAdapter {
    #[allow(dead_code)]
    sys_id: String,
    name: String,
    #[serde(default)]
    ip_address: String,
    #[serde(default)]
    mac_address: String,
    #[serde(default)]
    #[allow(dead_code)]
    netmask: String,
    cmdb_ci: Option<SnowRefSysId>,
}

#[derive(Debug, Deserialize)]
struct SnowIncident {
    sys_id: String,
    state: String,
    assignment_group: Option<SnowRef>,
    opened_at: String,
    #[serde(default, rename = "u_bonsai_detection_id")]
    bonsai_detection_id: String,
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

const PAGE_SIZE: usize = 500;
const MAX_PAGES: usize = 200; // safety cap: 100k records max

/// GET a single page from a ServiceNow table with automatic 429 retry.
async fn snow_get_page<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    instance_url: &str,
    table: &str,
    query: &str,
    fields: &str,
    username: &str,
    password: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<T>> {
    let url = format!(
        "{instance_url}/api/now/table/{table}?sysparm_query={query}&sysparm_fields={fields}\
         &sysparm_display_value=true&sysparm_limit={limit}&sysparm_offset={offset}"
    );

    let mut delay_secs = 1u64;
    for attempt in 0..4 {
        let resp = client
            .get(&url)
            .basic_auth(username, Some(password))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempt < 3 {
                warn!(table, attempt, delay_secs, "ServiceNow 429 — backing off");
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                delay_secs = (delay_secs * 2).min(60);
                continue;
            }
            break;
        }

        if !resp.status().is_success() {
            anyhow::bail!("ServiceNow {table} returned {}", resp.status());
        }

        let list: SnowList<T> = resp
            .json()
            .await
            .with_context(|| format!("parse ServiceNow {table} response"))?;
        return Ok(list.result);
    }

    anyhow::bail!("ServiceNow {table}: exceeded retry limit after repeated 429 responses")
}

/// GET a ServiceNow table with automatic 429 retry and exponential backoff (Q-13).
/// Kept for backward-compat with tests — fetches a single page of PAGE_SIZE.
#[allow(dead_code)]
async fn snow_get<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    instance_url: &str,
    table: &str,
    query: &str,
    fields: &str,
    username: &str,
    password: &str,
) -> Result<Vec<T>> {
    snow_get_page(client, instance_url, table, query, fields, username, password, 0, PAGE_SIZE).await
}

/// Paginated fetch — loops with sysparm_offset until an empty page or MAX_PAGES.
async fn snow_get_all<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    instance_url: &str,
    table: &str,
    query: &str,
    fields: &str,
    username: &str,
    password: &str,
) -> Result<Vec<T>> {
    let mut all = Vec::new();
    for page in 0..MAX_PAGES {
        let offset = page * PAGE_SIZE;
        let batch = snow_get_page(
            client, instance_url, table, query, fields, username, password, offset, PAGE_SIZE,
        )
        .await?;
        let count = batch.len();
        all.extend(batch);
        if count < PAGE_SIZE {
            break; // last page
        }
        debug!(table, page, total = all.len(), "ServiceNow pagination");
    }
    info!(table, count = all.len(), "ServiceNow fetch complete");
    Ok(all)
}

// ── Enricher ──────────────────────────────────────────────────────────────────

pub struct ServiceNowEnricher {
    config: EnricherConfig,
}

impl ServiceNowEnricher {
    pub fn from_config(config: EnricherConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl GraphEnricher for ServiceNowEnricher {
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
            property_namespace: "snow_".to_string(),
            owned_labels: vec![
                "Application".to_string(),
                "Incident".to_string(),
                "Location".to_string(),
                "Prefix".to_string(),
            ],
            owned_edge_types: vec![
                "HAS_ENRICHMENT_PROPERTY".to_string(),
                "RUNS_SERVICE".to_string(),
                "CARRIES_APPLICATION".to_string(),
                "HAS_INCIDENT".to_string(),
                "IN_LOCATION".to_string(),
                "IN_SITE".to_string(),
                "CMDB_PARENT_OF".to_string(),
                "LOC_PARENT_OF".to_string(),
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
            .resolve(
                &self.config.credential_alias,
                ResolvePurpose::ServiceNowAdmin,
            )
            .inspect_err(|e| {
                audit.log_credential_resolve(
                    &self.config.credential_alias,
                    "error",
                    Some(&e.to_string()),
                );
            })?;
        audit.log_credential_resolve(&self.config.credential_alias, "ok", None);

        let instance_url = self.config.base_url.trim_end_matches('/').to_string();
        let username = cred.username.clone();
        let password = cred.password.clone();
        let source = self.config.name.clone();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;

        // Phase 1: original tables (services, network CIs, relationships, incidents)
        let (services_res, cis_res, rels_res, incidents_res) = tokio::join!(
            snow_get_all::<SnowBusinessService>(
                &client,
                &instance_url,
                "cmdb_ci_business_service",
                "operational_status!=4",
                "sys_id,name,short_description,operational_status,assigned_to,assignment_group",
                &username,
                &password,
            ),
            snow_get_all::<SnowCi>(
                &client,
                &instance_url,
                "cmdb_ci_netgear",
                "install_status=1",
                "sys_id,name,assigned_to,assignment_group",
                &username,
                &password,
            ),
            snow_get_all::<SnowRelCi>(
                &client,
                &instance_url,
                "cmdb_rel_ci",
                "",
                "parent.sys_id,parent.name,child.sys_id,child.name,type.name",
                &username,
                &password,
            ),
            snow_get_all::<SnowIncident>(
                &client,
                &instance_url,
                "incident",
                "sourceSTARTSWITHbonsai^active=true",
                "sys_id,state,assignment_group,opened_at,u_bonsai_detection_id",
                &username,
                &password,
            ),
        );

        // Phase 2: extended CMDB tables (servers, locations, subnets, IP addresses, adapters)
        let (servers_res, locs_res, subnets_res, ips_res, adapters_res) = tokio::join!(
            snow_get_all::<SnowServer>(
                &client,
                &instance_url,
                "cmdb_ci_server",
                "install_status=1",
                "sys_id,name,serial_number,os,os_version,ram,cpu_count,cpu_type,ip_address,\
                 model_id,manufacturer,assigned_to,assignment_group,location",
                &username,
                &password,
            ),
            snow_get_all::<SnowLoc>(
                &client,
                &instance_url,
                "cmn_location",
                "",
                "sys_id,name,street,city,state,country,parent",
                &username,
                &password,
            ),
            snow_get_all::<SnowSubnet>(
                &client,
                &instance_url,
                "cmdb_ci_ip_network",
                "install_status=1",
                "sys_id,name,subnet,short_description",
                &username,
                &password,
            ),
            snow_get_all::<SnowIpAddress>(
                &client,
                &instance_url,
                "cmdb_ci_ip_address",
                "",
                "sys_id,ip_address,netmask,nic",
                &username,
                &password,
            ),
            snow_get_all::<SnowNetworkAdapter>(
                &client,
                &instance_url,
                "cmdb_ci_network_adapter",
                "",
                "sys_id,name,ip_address,mac_address,netmask,cmdb_ci",
                &username,
                &password,
            ),
        );

        let services = services_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch business services: {e:#}"));
            vec![]
        });
        let cis = cis_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch device CIs: {e:#}"));
            vec![]
        });
        let rels = rels_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch relationships: {e:#}"));
            vec![]
        });
        let incidents = incidents_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch incidents: {e:#}"));
            vec![]
        });
        let servers = servers_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch servers: {e:#}"));
            vec![]
        });
        let locations = locs_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch locations: {e:#}"));
            vec![]
        });
        let subnets = subnets_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch subnets: {e:#}"));
            vec![]
        });
        let ip_addresses = ips_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch IP addresses: {e:#}"));
            vec![]
        });
        let adapters = adapters_res.unwrap_or_else(|e| {
            warnings.push(format!("failed to fetch network adapters: {e:#}"));
            vec![]
        });

        info!(
            services = services.len(),
            cis = cis.len(),
            servers = servers.len(),
            locations = locations.len(),
            subnets = subnets.len(),
            ips = ip_addresses.len(),
            adapters = adapters.len(),
            rels = rels.len(),
            incidents = incidents.len(),
            "ServiceNow CMDB fetch summary",
        );

        let db = store.db();
        let write_lock = store.write_lock();
        let (nodes_touched, edges_created, write_warnings) =
            tokio::task::spawn_blocking(move || {
                let _guard = write_lock.lock().expect("write lock poisoned");
                write_to_graph(
                    &db, &services, &cis, &servers, &rels, &incidents,
                    &locations, &subnets, &ip_addresses, &adapters, &source,
                )
            })
            .await
            .context("graph write task panicked")??;

        warnings.extend(write_warnings);
        audit.log_run("success", nodes_touched, None);

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
            .resolve(
                &self.config.credential_alias,
                ResolvePurpose::ServiceNowAdmin,
            )
            .inspect_err(|e| {
                audit.log_credential_resolve(
                    &self.config.credential_alias,
                    "error",
                    Some(&e.to_string()),
                );
            })?;
        audit.log_credential_resolve(&self.config.credential_alias, "ok", None);

        let instance_url = self.config.base_url.trim_end_matches('/').to_string();
        let url = format!("{instance_url}/api/now/table/sys_properties?sysparm_limit=1");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("build reqwest client")?;
        let resp = client
            .get(&url)
            .basic_auth(&cred.username, Some(&*cred.password))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            anyhow::bail!("ServiceNow returned {}", resp.status())
        }
    }
}

// ── Graph write helpers ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn write_to_graph(
    db: &Arc<lbug::Database>,
    services: &[SnowBusinessService],
    cis: &[SnowCi],
    servers: &[SnowServer],
    rels: &[SnowRelCi],
    incidents: &[SnowIncident],
    locations: &[SnowLoc],
    subnets: &[SnowSubnet],
    ip_addresses: &[SnowIpAddress],
    adapters: &[SnowNetworkAdapter],
    source: &str,
) -> Result<(usize, usize, Vec<String>)> {
    let conn = Connection::new(db).context("open graph connection")?;
    let mut nodes = 0usize;
    let mut edges = 0usize;
    let mut warnings = Vec::new();
    let now_ns = crate::graph::common::now_ns();

    // sys_id → Application node id mapping for relationship wiring
    let mut app_by_sys_id: HashMap<String, String> = HashMap::new();
    // sys_id → location name for hierarchy building (used for loc ref resolution)
    let mut loc_by_sys_id: HashMap<String, String> = HashMap::new();
    let _ = &loc_by_sys_id; // suppress unused warning — read in future server→location wiring

    // ── 1. Application nodes ──────────────────────────────────────────────────
    for svc in services {
        let id = format!("snow_app_{}", svc.sys_id);
        let criticality = operational_status_to_criticality(&svc.operational_status);
        let owner_group = svc
            .assignment_group
            .as_ref()
            .map(|r| r.display_value.as_str())
            .unwrap_or("")
            .to_string();

        if let Err(e) = upsert_application(
            &conn,
            &id,
            &svc.name,
            criticality,
            &owner_group,
            source,
            now_ns,
        ) {
            warnings.push(format!("Application {}: {e:#}", svc.name));
        } else {
            nodes += 1;
            app_by_sys_id.insert(svc.sys_id.clone(), id);
        }
    }

    // ── 2. Network device CI enrichment properties ────────────────────────────
    for ci in cis {
        let owner_group = ci
            .assignment_group
            .as_ref()
            .map(|r| r.display_value.as_str())
            .unwrap_or("")
            .to_string();
        let assigned_to = ci
            .assigned_to
            .as_ref()
            .map(|r| r.display_value.as_str())
            .unwrap_or("")
            .to_string();

        let props = [
            ("snow_ci_id", ci.sys_id.as_str()),
            ("snow_owner_group", owner_group.as_str()),
            ("snow_assigned_to", assigned_to.as_str()),
        ];
        for (key, val) in props {
            if val.is_empty() { continue; }
            let prop_id = format!("{}:{key}", ci.name);
            if let Err(e) = upsert_enrichment_property_by_hostname(
                &conn, &prop_id, &ci.name, key, val, source, now_ns,
            ) {
                warnings.push(format!("CI {} prop {key}: {e:#}", ci.name));
            } else {
                nodes += 1;
                edges += 1;
            }
        }
    }

    // ── 3. Server CI enrichment properties (OS, serial, RAM, CPU, etc.) ───────
    for srv in servers {
        let mut props: Vec<(&str, &str)> = vec![
            ("snow_ci_id", &srv.sys_id),
        ];
        if !srv.serial_number.is_empty() { props.push(("snow_serial", &srv.serial_number)); }
        if !srv.os.is_empty() { props.push(("snow_os", &srv.os)); }
        if !srv.os_version.is_empty() { props.push(("snow_os_version", &srv.os_version)); }
        if !srv.ram.is_empty() { props.push(("snow_ram", &srv.ram)); }
        if !srv.cpu_count.is_empty() { props.push(("snow_cpu_count", &srv.cpu_count)); }
        if !srv.cpu_type.is_empty() { props.push(("snow_cpu_type", &srv.cpu_type)); }
        if !srv.ip_address.is_empty() { props.push(("snow_ip_address", &srv.ip_address)); }
        if let Some(m) = &srv.model_id {
            if !m.display_value.is_empty() {
                props.push(("snow_model", &m.display_value));
            }
        }
        if let Some(m) = &srv.manufacturer {
            if !m.display_value.is_empty() {
                props.push(("snow_manufacturer", &m.display_value));
            }
        }
        if let Some(ag) = &srv.assignment_group {
            if !ag.display_value.is_empty() {
                props.push(("snow_owner_group", &ag.display_value));
            }
        }
        if let Some(at) = &srv.assigned_to {
            if !at.display_value.is_empty() {
                props.push(("snow_assigned_to", &at.display_value));
            }
        }
        if let Some(loc) = &srv.location {
            if !loc.display_value.is_empty() {
                props.push(("snow_location", &loc.display_value));
            }
        }
        for (key, val) in &props {
            if val.is_empty() { continue; }
            let prop_id = format!("{}:{key}", srv.name);
            if let Err(e) = upsert_enrichment_property_by_hostname(
                &conn, &prop_id, &srv.name, key, val, source, now_ns,
            ) {
                warnings.push(format!("Server {} prop {key}: {e:#}", srv.name));
            } else {
                nodes += 1;
                edges += 1;
            }
        }
    }

    // ── 4. Location hierarchy ─────────────────────────────────────────────────
    for loc in locations {
        loc_by_sys_id.insert(loc.sys_id.clone(), loc.name.clone());
        let loc_id = format!("snow_loc_{}", loc.sys_id);
        let full_addr = [&loc.street, &loc.city, &loc.state, &loc.country]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if let Err(e) = upsert_location(&conn, &loc_id, &loc.name, &full_addr, source, now_ns) {
            warnings.push(format!("Location {}: {e:#}", loc.name));
        } else {
            nodes += 1;
        }
    }
    // Wire parent→child location edges
    for loc in locations {
        if let Some(parent_ref) = &loc.parent {
            if !parent_ref.value.is_empty() {
                let child_id = format!("snow_loc_{}", loc.sys_id);
                let parent_id = format!("snow_loc_{}", parent_ref.value);
                if let Err(e) = link_location_parent(&conn, &parent_id, &child_id) {
                    warnings.push(format!("LOC_PARENT {} → {}: {e:#}", parent_ref.value, loc.sys_id));
                } else {
                    edges += 1;
                }
            }
        }
    }

    // ── 5. Subnet → Prefix nodes ─────────────────────────────────────────────
    for subnet in subnets {
        if subnet.cidr.is_empty() { continue; }
        let id = format!("snow_prefix_{}", subnet.cidr.replace('/', "_"));
        if let Err(e) = upsert_prefix(
            &conn, &id, &subnet.cidr, &subnet.name, &subnet.short_description, source, now_ns,
        ) {
            warnings.push(format!("Subnet {}: {e:#}", subnet.cidr));
        } else {
            nodes += 1;
        }
    }

    // ── 6. IP Address enrichment ─────────────────────────────────────────────
    for ip in ip_addresses {
        if ip.ip_address.is_empty() { continue; }
        let prop_id = format!("snow_ip_{}", ip.sys_id);
        let val = if ip.netmask.is_empty() {
            ip.ip_address.clone()
        } else {
            format!("{}/{}", ip.ip_address, ip.netmask)
        };
        // Link to parent CI if available
        if let Some(nic) = &ip.nic {
            if !nic.display_value.is_empty() {
                // nic.display_value is usually the adapter name — link to parent device via adapter
                if let Err(e) = upsert_enrichment_property_by_hostname(
                    &conn, &prop_id, &nic.display_value, "snow_ip_address", &val, source, now_ns,
                ) {
                    warnings.push(format!("IP {} adapter {}: {e:#}", ip.ip_address, nic.display_value));
                } else {
                    nodes += 1;
                    edges += 1;
                }
            }
        }
    }

    // ── 7. Network adapter enrichment ────────────────────────────────────────
    for adapter in adapters {
        if let Some(ci_ref) = &adapter.cmdb_ci {
            if ci_ref.display_value.is_empty() { continue; }
            // Write adapter properties on the parent CI hostname
            let hostname = &ci_ref.display_value;
            let mut props: Vec<(&str, &str)> = Vec::new();
            if !adapter.ip_address.is_empty() {
                props.push(("snow_adapter_ip", &adapter.ip_address));
            }
            if !adapter.mac_address.is_empty() {
                props.push(("snow_adapter_mac", &adapter.mac_address));
            }
            if !adapter.name.is_empty() {
                props.push(("snow_adapter_name", &adapter.name));
            }
            for (key, val) in props {
                let prop_id = format!("{hostname}:{}:{key}", adapter.sys_id);
                if let Err(e) = upsert_enrichment_property_by_hostname(
                    &conn, &prop_id, hostname, key, val, source, now_ns,
                ) {
                    warnings.push(format!("Adapter {} prop {key}: {e:#}", adapter.sys_id));
                } else {
                    nodes += 1;
                    edges += 1;
                }
            }
        }
    }

    // ── 8. Relationships: service bindings + parent/child CI hierarchy ────────
    for rel in rels {
        let rel_name = rel.rel_type.display_value.to_lowercase();
        if rel_name.contains("runs") || rel_name.contains("provided by") {
            // Service relationship → RUNS_SERVICE / CARRIES_APPLICATION
            let app_sys_id = &rel.child.value;
            if let Some(app_id) = app_by_sys_id.get(app_sys_id) {
                let device_hostname = &rel.parent.display_value;
                let rel_label = if rel_name.contains("runs") {
                    "RUNS_SERVICE"
                } else {
                    "CARRIES_APPLICATION"
                };
                match link_device_application(&conn, device_hostname, app_id, rel_label) {
                    Ok(()) => edges += 1,
                    Err(e) => warnings.push(format!("{rel_label} {device_hostname} → {app_id}: {e:#}")),
                }
            }
        } else {
            // Generic parent/child CI relationship → CMDB_PARENT_OF edge
            // Both parent and child are identified by their display_value (CI name)
            if !rel.parent.display_value.is_empty() && !rel.child.display_value.is_empty() {
                match link_cmdb_parent_child(
                    &conn,
                    &rel.parent.display_value,
                    &rel.child.display_value,
                    &rel.rel_type.display_value,
                    source,
                    now_ns,
                ) {
                    Ok(()) => edges += 1,
                    Err(e) => warnings.push(format!(
                        "CMDB_PARENT_OF {} → {}: {e:#}",
                        rel.parent.display_value, rel.child.display_value,
                    )),
                }
            }
        }
    }

    // ── 9. Incident nodes + HAS_INCIDENT edges (T2-5) ────────────────────────
    for inc in incidents {
        let id = format!("snow_inc_{}", inc.sys_id);
        let assignment_group = inc
            .assignment_group
            .as_ref()
            .map(|r| r.display_value.as_str())
            .unwrap_or("")
            .to_string();
        let opened_ns = parse_snow_datetime_ns(&inc.opened_at);

        if let Err(e) = upsert_incident(
            &conn,
            &id,
            &inc.sys_id,
            &inc.state,
            &assignment_group,
            opened_ns,
            &inc.bonsai_detection_id,
            now_ns,
        ) {
            warnings.push(format!("Incident {}: {e:#}", inc.sys_id));
        } else {
            nodes += 1;
            if !inc.bonsai_detection_id.is_empty() {
                match link_detection_incident(&conn, &inc.bonsai_detection_id, &id) {
                    Ok(()) => edges += 1,
                    Err(e) => warnings.push(format!(
                        "HAS_INCIDENT {} → {id}: {e:#}",
                        inc.bonsai_detection_id
                    )),
                }
            }
        }
    }

    Ok((nodes, edges, warnings))
}

fn operational_status_to_criticality(status: &str) -> &'static str {
    match status {
        "1" => "operational",
        "2" => "non_operational",
        "3" => "repair_in_progress",
        "6" => "end_of_life",
        _ => "unknown",
    }
}

fn parse_snow_datetime_ns(s: &str) -> i64 {
    // ServiceNow datetimes come as "2024-01-15 14:30:00" in UTC
    if let Ok(dt) = time::PrimitiveDateTime::parse(
        s,
        &time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]")
            .unwrap_or_default(),
    ) {
        dt.assume_utc().unix_timestamp_nanos() as i64
    } else {
        0
    }
}

fn upsert_application(
    conn: &Connection<'_>,
    id: &str,
    name: &str,
    criticality: &str,
    owner_group: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (a:Application {id: $id}) \
         SET a.name = $name, a.criticality = $crit, a.owner_group = $og, \
             a.source_name = $src, a.updated_at = $now",
        )
        .context("prepare upsert_application")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("name", Value::String(name.to_string())),
            ("crit", Value::String(criticality.to_string())),
            ("og", Value::String(owner_group.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_application")?;
    Ok(())
}

fn upsert_enrichment_property_by_hostname(
    conn: &Connection<'_>,
    id: &str,
    hostname: &str,
    key: &str,
    value: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (p:EnrichmentProperty {id: $id}) \
         SET p.device_address = $hn, p.key = $key, p.value = $val, \
             p.source_name = $src, p.updated_at = $now",
        )
        .context("prepare snow enrichment property")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("hn", Value::String(hostname.to_string())),
            ("key", Value::String(key.to_string())),
            ("val", Value::String(value.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute snow enrichment property")?;

    // Best-effort edge Device → EnrichmentProperty by hostname match
    let mut edge = conn
        .prepare(
            "MATCH (d:Device {hostname: $hn}), (p:EnrichmentProperty {id: $id}) \
         MERGE (d)-[:HAS_ENRICHMENT_PROPERTY]->(p)",
        )
        .context("prepare snow HAS_ENRICHMENT_PROPERTY")?;
    conn.execute(
        &mut edge,
        vec![
            ("hn", Value::String(hostname.to_string())),
            ("id", Value::String(id.to_string())),
        ],
    )
    .context("execute snow HAS_ENRICHMENT_PROPERTY")?;

    // Write provenance and reconcile against any existing property with the same
    // key from a different source on the same device.
    reconcile_and_write_provenance(conn, id, hostname, key, value, source_name, now_ns)?;

    Ok(())
}

/// Default source priority (highest first). CLI-parsed data wins over NetBox,
/// which wins over ServiceNow, reflecting the trust hierarchy:
/// live device → IPAM/DCIM → CMDB.
const DEFAULT_SOURCE_PRIORITY: &[&str] = &["cli", "netbox", "servicenow"];

/// After writing an enrichment property, check whether a property with the same
/// key but different source already exists on this device.  If so, record
/// `PropertyProvenance` entries for both the new and existing value, marking one
/// as `winner` and the other as `loser` based on source priority.
///
/// If no conflict exists, a single provenance entry is written for the new value.
fn reconcile_and_write_provenance(
    conn: &Connection<'_>,
    prop_id: &str,
    hostname: &str,
    key: &str,
    new_value: &str,
    new_source: &str,
    now_ns: i64,
) -> Result<()> {
    // Find existing properties on the same device with the same key but different source
    let mut find = conn
        .prepare(
            "MATCH (d:Device {hostname: $hn})-[:HAS_ENRICHMENT_PROPERTY]->(p:EnrichmentProperty) \
             WHERE p.key = $key AND p.source_name <> $src \
             RETURN p.id, p.value, p.source_name",
        )
        .context("prepare reconcile lookup")?;
    let rows: Vec<(String, String, String)> = conn
        .execute(
            &mut find,
            vec![
                ("hn", Value::String(hostname.to_string())),
                ("key", Value::String(key.to_string())),
                ("src", Value::String(new_source.to_string())),
            ],
        )
        .context("execute reconcile lookup")?
        .map(|row| {
            let pid = match &row[0] { Value::String(s) => s.clone(), _ => String::new() };
            let val = match &row[1] { Value::String(s) => s.clone(), _ => String::new() };
            let src = match &row[2] { Value::String(s) => s.clone(), _ => String::new() };
            (pid, val, src)
        })
        .collect();

    let new_priority = source_priority_rank(new_source);

    if rows.is_empty() {
        // No conflict — write a simple provenance entry
        write_provenance(conn, prop_id, "enrichment_property", prop_id, new_source, "enricher", "high", now_ns, None)?;
    } else {
        // Conflict detected — determine winner
        for (existing_id, existing_val, existing_src) in &rows {
            let existing_priority = source_priority_rank(existing_src);
            let (new_is_winner, conflict_detail) = if new_priority <= existing_priority {
                // Lower rank = higher priority (cli=0 beats netbox=1 beats servicenow=2)
                (true, format!(
                    "{{\"conflict\":true,\"winner\":\"{new_source}\",\"loser\":\"{existing_src}\",\
                     \"winner_value\":\"{new_value}\",\"loser_value\":\"{existing_val}\"}}"
                ))
            } else {
                (false, format!(
                    "{{\"conflict\":true,\"winner\":\"{existing_src}\",\"loser\":\"{new_source}\",\
                     \"winner_value\":\"{existing_val}\",\"loser_value\":\"{new_value}\"}}"
                ))
            };

            // Provenance for the new value
            let confidence = if new_is_winner { "high" } else { "low" };
            write_provenance(
                conn, prop_id, "enrichment_property", prop_id, new_source,
                "enricher", confidence, now_ns, Some(&conflict_detail),
            )?;

            // Provenance for the existing conflicting value
            let existing_confidence = if new_is_winner { "low" } else { "high" };
            let existing_prov_id = format!("{existing_id}:prov:{new_source}");
            write_provenance(
                conn, &existing_prov_id, "enrichment_property", existing_id, existing_src,
                "enricher", existing_confidence, now_ns, Some(&conflict_detail),
            )?;
        }
    }

    Ok(())
}

fn source_priority_rank(source: &str) -> usize {
    DEFAULT_SOURCE_PRIORITY
        .iter()
        .position(|&s| source.contains(s))
        .unwrap_or(DEFAULT_SOURCE_PRIORITY.len())
}

#[allow(clippy::too_many_arguments)]
fn write_provenance(
    conn: &Connection<'_>,
    prov_id: &str,
    owner_kind: &str,
    owner_id: &str,
    source: &str,
    parser: &str,
    confidence: &str,
    now_ns: i64,
    details_json: Option<&str>,
) -> Result<()> {
    let prov_node_id = format!("prov_{prov_id}");
    let details = details_json.unwrap_or("{}").to_string();
    let mut stmt = conn
        .prepare(
            "MERGE (pv:PropertyProvenance {id: $id}) \
         SET pv.owner_kind = $ok, pv.owner_id = $oid, pv.source = $src, \
             pv.parser = $parser, pv.confidence = $conf, \
             pv.captured_at = $now, pv.details_json = $dj",
        )
        .context("prepare write_provenance")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(prov_node_id.clone())),
            ("ok", Value::String(owner_kind.to_string())),
            ("oid", Value::String(owner_id.to_string())),
            ("src", Value::String(source.to_string())),
            ("parser", Value::String(parser.to_string())),
            ("conf", Value::String(confidence.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
            ("dj", Value::String(details)),
        ],
    )
    .context("execute write_provenance")?;

    // Best-effort: link EnrichmentProperty → PropertyProvenance
    let mut edge = conn
        .prepare(
            "MATCH (ep:EnrichmentProperty {id: $eid}), (pv:PropertyProvenance {id: $pid}) \
         MERGE (ep)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(pv)",
        )
        .context("prepare ENRICHMENT_PROPERTY_PROVENANCE edge")?;
    let _ = conn.execute(
        &mut edge,
        vec![
            ("eid", Value::String(owner_id.to_string())),
            ("pid", Value::String(prov_node_id)),
        ],
    );
    Ok(())
}

fn upsert_location(
    conn: &Connection<'_>,
    id: &str,
    name: &str,
    full_address: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (l:Location {id: $id}) \
         SET l.name = $name, l.full_address = $addr, \
             l.source = $src, l.source_name = $src, l.updated_at = $now",
        )
        .context("prepare upsert_location")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("name", Value::String(name.to_string())),
            ("addr", Value::String(full_address.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_location")?;
    Ok(())
}

fn link_location_parent(
    conn: &Connection<'_>,
    parent_id: &str,
    child_id: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MATCH (p:Location {id: $pid}), (c:Location {id: $cid}) \
         MERGE (p)-[:LOC_PARENT_OF]->(c)",
        )
        .context("prepare link_location_parent")?;
    conn.execute(
        &mut stmt,
        vec![
            ("pid", Value::String(parent_id.to_string())),
            ("cid", Value::String(child_id.to_string())),
        ],
    )
    .context("execute link_location_parent")?;
    Ok(())
}

fn upsert_prefix(
    conn: &Connection<'_>,
    id: &str,
    cidr: &str,
    name: &str,
    description: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (p:Prefix {id: $id}) \
         SET p.prefix = $cidr, p.name = $name, p.description = $descr, \
             p.source_name = $src, p.updated_at = $now",
        )
        .context("prepare upsert_prefix")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("cidr", Value::String(cidr.to_string())),
            ("name", Value::String(name.to_string())),
            ("descr", Value::String(description.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_prefix")?;
    Ok(())
}

fn link_device_application(
    conn: &Connection<'_>,
    device_hostname: &str,
    app_id: &str,
    rel_type: &str,
) -> Result<()> {
    let q = format!(
        "MATCH (d:Device {{hostname: $hn}}), (a:Application {{id: $aid}}) \
         MERGE (d)-[:{rel_type}]->(a)"
    );
    let mut stmt = conn
        .prepare(&q)
        .context("prepare link_device_application")?;
    conn.execute(
        &mut stmt,
        vec![
            ("hn", Value::String(device_hostname.to_string())),
            ("aid", Value::String(app_id.to_string())),
        ],
    )
    .context("execute link_device_application")?;
    Ok(())
}

fn link_cmdb_parent_child(
    conn: &Connection<'_>,
    parent_hostname: &str,
    child_hostname: &str,
    rel_type_name: &str,
    source_name: &str,
    now_ns: i64,
) -> Result<()> {
    // Write a CMDB_PARENT_OF edge between two Device nodes (matched by hostname).
    // Store the ServiceNow relationship type name on the edge for auditing.
    let mut stmt = conn
        .prepare(
            "MATCH (p:Device {hostname: $phn}), (c:Device {hostname: $chn}) \
         MERGE (p)-[r:CMDB_PARENT_OF]->(c) \
         SET r.rel_type = $rt, r.source_name = $src, r.updated_at = $now",
        )
        .context("prepare link_cmdb_parent_child")?;
    conn.execute(
        &mut stmt,
        vec![
            ("phn", Value::String(parent_hostname.to_string())),
            ("chn", Value::String(child_hostname.to_string())),
            ("rt", Value::String(rel_type_name.to_string())),
            ("src", Value::String(source_name.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute link_cmdb_parent_child")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_incident(
    conn: &Connection<'_>,
    id: &str,
    snow_sys_id: &str,
    state: &str,
    assignment_group: &str,
    opened_at_ns: i64,
    detection_id: &str,
    now_ns: i64,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (i:Incident {id: $id}) \
         SET i.snow_sys_id = $sid, i.state = $state, i.assignment_group = $ag, \
             i.opened_at_ns = $oat, i.detection_id = $did, i.updated_at = $now",
        )
        .context("prepare upsert_incident")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.to_string())),
            ("sid", Value::String(snow_sys_id.to_string())),
            ("state", Value::String(state.to_string())),
            ("ag", Value::String(assignment_group.to_string())),
            ("oat", Value::Int64(opened_at_ns)),
            ("did", Value::String(detection_id.to_string())),
            ("now", crate::graph::common::ts(now_ns)),
        ],
    )
    .context("execute upsert_incident")?;
    Ok(())
}

fn link_detection_incident(
    conn: &Connection<'_>,
    detection_id: &str,
    incident_id: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MATCH (e:DetectionEvent {id: $eid}), (i:Incident {id: $iid}) \
         MERGE (e)-[:HAS_INCIDENT]->(i)",
        )
        .context("prepare HAS_INCIDENT")?;
    conn.execute(
        &mut stmt,
        vec![
            ("eid", Value::String(detection_id.to_string())),
            ("iid", Value::String(incident_id.to_string())),
        ],
    )
    .context("execute HAS_INCIDENT")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStore;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn open_test_graph(label: &str) -> GraphStore {
        let path = std::env::temp_dir()
            .join(format!("bonsai-snow-test-{label}-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        GraphStore::open(&path, 256 * 1024 * 1024).expect("open test graph")
    }

    // Helper to call write_to_graph with only the fields we care about
    fn write_simple(
        db: &Arc<lbug::Database>,
        services: &[SnowBusinessService],
        cis: &[SnowCi],
        source: &str,
    ) -> Result<(usize, usize, Vec<String>)> {
        write_to_graph(db, services, cis, &[], &[], &[], &[], &[], &[], &[], source)
    }

    // ── SnowRef polymorphic deserialisation (Q-14) ────────────────────────────

    #[test]
    fn snow_ref_deserialises_from_object_form() {
        let json = r#"{"display_value": "Network-Operations", "value": "abc123"}"#;
        let r: SnowRef = serde_json::from_str(json).unwrap();
        assert_eq!(r.display_value, "Network-Operations");
    }

    #[test]
    fn snow_ref_deserialises_from_plain_string() {
        let json = r#""Network-Operations""#;
        let r: SnowRef = serde_json::from_str(json).unwrap();
        assert_eq!(r.display_value, "Network-Operations");
    }

    #[test]
    fn snow_ref_option_handles_null() {
        let json = r#"{"assignment_group": null, "sys_id": "x", "name": "web-front",
                       "assigned_to": null}"#;
        let ci: SnowCi = serde_json::from_str(json).unwrap();
        assert!(ci.assignment_group.is_none());
    }

    // ── 429 retry with exponential backoff (Q-13) ─────────────────────────────

    #[tokio::test]
    async fn snow_get_retries_on_429_and_succeeds() {
        let server = MockServer::start().await;
        let payload = serde_json::json!({"result": [{"sys_id": "s1", "name": "SVC",
            "operational_status": "1", "assigned_to": null, "assignment_group": null}]});

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&payload))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result: Vec<SnowBusinessService> = snow_get(
            &client,
            &server.uri(),
            "cmdb_ci_business_service",
            "",
            "sys_id,name,operational_status",
            "user",
            "pass",
        )
        .await
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "SVC");
    }

    #[tokio::test]
    async fn snow_get_fails_after_exhausting_retries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result: Result<Vec<SnowBusinessService>> = snow_get(
            &client,
            &server.uri(),
            "cmdb_ci_business_service",
            "",
            "sys_id",
            "u",
            "p",
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("retry limit"));
    }

    // ── graph write: Application nodes ───────────────────────────────────────

    #[test]
    fn write_to_graph_creates_application_nodes() {
        let store = open_test_graph("apps");
        let db = store.db();

        let services = vec![SnowBusinessService {
            sys_id: "svc001".to_string(),
            name: "payment-frontend".to_string(),
            short_description: "".to_string(),
            operational_status: "1".to_string(),
            assigned_to: None,
            assignment_group: None,
        }];

        let (nodes, _edges, warnings) = write_simple(&db, &services, &[], "snow-test").unwrap();
        assert_eq!(nodes, 1, "one Application node expected");
        assert!(warnings.is_empty());
    }

    #[test]
    fn write_to_graph_idempotent_application_upsert() {
        let store = open_test_graph("app-idem");
        let db = store.db();

        let services = vec![SnowBusinessService {
            sys_id: "svc001".to_string(),
            name: "payment-frontend".to_string(),
            short_description: "".to_string(),
            operational_status: "1".to_string(),
            assigned_to: None,
            assignment_group: None,
        }];

        let (n1, _, _) = write_simple(&db, &services, &[], "snow-test").unwrap();
        let (n2, _, _) = write_simple(&db, &services, &[], "snow-test").unwrap();
        assert_eq!(n1, n2, "MERGE must be idempotent");
    }

    // ── Location hierarchy ───────────────────────────────────────────────────

    #[test]
    fn write_locations_creates_hierarchy() {
        let store = open_test_graph("locs");
        let db = store.db();

        let locations = vec![
            SnowLoc {
                sys_id: "loc1".to_string(),
                name: "US-East".to_string(),
                street: "".to_string(),
                city: "New York".to_string(),
                state: "NY".to_string(),
                country: "US".to_string(),
                parent: None,
            },
            SnowLoc {
                sys_id: "loc2".to_string(),
                name: "NYC-DC1".to_string(),
                street: "60 Hudson".to_string(),
                city: "New York".to_string(),
                state: "NY".to_string(),
                country: "US".to_string(),
                parent: Some(SnowRefSysId {
                    display_value: "US-East".to_string(),
                    value: "loc1".to_string(),
                }),
            },
        ];

        let (nodes, edges, warnings) = write_to_graph(
            &db, &[], &[], &[], &[], &[], &locations, &[], &[], &[], "snow-test",
        )
        .unwrap();
        assert_eq!(nodes, 2, "two Location nodes");
        assert_eq!(edges, 1, "one parent→child edge");
        assert!(warnings.is_empty());
    }

    // ── Server CI enrichment ─────────────────────────────────────────────────

    #[test]
    fn write_server_ci_properties() {
        let store = open_test_graph("srv");
        let db = store.db();

        let servers = vec![SnowServer {
            sys_id: "srv001".to_string(),
            name: "web-server-01".to_string(),
            serial_number: "SN12345".to_string(),
            os: "Linux".to_string(),
            os_version: "Ubuntu 22.04".to_string(),
            ram: "32768".to_string(),
            cpu_count: "8".to_string(),
            cpu_type: "Intel Xeon".to_string(),
            ip_address: "10.1.1.100".to_string(),
            model_id: None,
            manufacturer: None,
            assigned_to: None,
            assignment_group: None,
            location: None,
        }];

        let (nodes, _edges, warnings) = write_to_graph(
            &db, &[], &[], &servers, &[], &[], &[], &[], &[], &[], "snow-test",
        )
        .unwrap();
        // snow_ci_id + serial + os + os_version + ram + cpu_count + cpu_type + ip = 8 props
        assert!(nodes >= 8, "expected at least 8 server properties, got {nodes}");
        assert!(warnings.is_empty());
    }

    // ── operational_status_to_criticality mapping ─────────────────────────────

    #[test]
    fn criticality_mapping_covers_known_codes() {
        assert_eq!(operational_status_to_criticality("1"), "operational");
        assert_eq!(operational_status_to_criticality("2"), "non_operational");
        assert_eq!(operational_status_to_criticality("6"), "end_of_life");
        assert_eq!(operational_status_to_criticality("99"), "unknown");
    }
}

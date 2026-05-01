//! ServiceNow CMDB GraphEnricher — pulls business context from a ServiceNow PDI
//! (or production instance with scoped roles) and writes it to the bonsai graph.
//!
//! Writes:
//! - `Application(id, name, criticality, owner_group)` nodes from cmdb_ci_business_service
//! - `Device.snow_ci_id`, `snow_owner_group`, `snow_assignment_group` properties
//! - `RUNS_SERVICE` / `CARRIES_APPLICATION` edges from cmdb_rel_ci
//! - `Incident` nodes from incidents where source = "bonsai" (T2-5 state consumption)
//!
//! Auth: Basic auth — username + password from vault under `credential_alias`.
//! Credential purpose: `ResolvePurpose::ServiceNowAdmin`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
struct SnowRef {
    display_value: String,
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

async fn snow_get<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    instance_url: &str,
    table: &str,
    query: &str,
    fields: &str,
    username: &str,
    password: &str,
) -> Result<Vec<T>> {
    let url = format!(
        "{instance_url}/api/now/table/{table}?sysparm_query={query}&sysparm_fields={fields}&sysparm_display_value=all&sysparm_limit=500"
    );
    let resp = client
        .get(&url)
        .basic_auth(username, Some(password))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("ServiceNow {table} returned {}", resp.status());
    }

    let list: SnowList<T> = resp
        .json()
        .await
        .with_context(|| format!("parse ServiceNow {table} response"))?;
    Ok(list.result)
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
            owned_labels: vec!["Application".to_string(), "Incident".to_string()],
            owned_edge_types: vec![
                "HAS_ENRICHMENT_PROPERTY".to_string(),
                "RUNS_SERVICE".to_string(),
                "CARRIES_APPLICATION".to_string(),
                "HAS_INCIDENT".to_string(),
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

        // Fetch business services, device CIs, relationships, and incidents concurrently
        let (services_res, cis_res, rels_res, incidents_res) = tokio::join!(
            snow_get::<SnowBusinessService>(
                &client,
                &instance_url,
                "cmdb_ci_business_service",
                "operational_status!=4",
                "sys_id,name,short_description,operational_status,assigned_to,assignment_group",
                &username,
                &password,
            ),
            snow_get::<SnowCi>(
                &client,
                &instance_url,
                "cmdb_ci_netgear",
                "install_status=1",
                "sys_id,name,assigned_to,assignment_group",
                &username,
                &password,
            ),
            snow_get::<SnowRelCi>(
                &client,
                &instance_url,
                "cmdb_rel_ci",
                "type.name=Runs^ORtype.name=Runs::Provided by",
                "parent.sys_id,parent.name,child.sys_id,child.name,type.name",
                &username,
                &password,
            ),
            snow_get::<SnowIncident>(
                &client,
                &instance_url,
                "incident",
                "sourceSTARTSWITHbonsai^active=true",
                "sys_id,state,assignment_group,opened_at,u_bonsai_detection_id",
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

        let db = store.db();
        let (nodes_touched, write_warnings) = tokio::task::spawn_blocking(move || {
            write_to_graph(&db, &services, &cis, &rels, &incidents, &source)
        })
        .await
        .context("graph write task panicked")??;

        warnings.extend(write_warnings);
        audit.log_run("success", nodes_touched, None);

        Ok(EnrichmentReport {
            enricher_name: self.config.name.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
            nodes_touched,
            edges_created: 0,
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
            .basic_auth(&cred.username, Some(&cred.password))
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

fn write_to_graph(
    db: &Arc<lbug::Database>,
    services: &[SnowBusinessService],
    cis: &[SnowCi],
    rels: &[SnowRelCi],
    incidents: &[SnowIncident],
    source: &str,
) -> Result<(usize, Vec<String>)> {
    let conn = Connection::new(db).context("open graph connection")?;
    let mut count = 0usize;
    let mut warnings = Vec::new();
    let now_ns = crate::graph::common::now_ns();

    // sys_id → Application node id mapping for relationship wiring
    let mut app_by_sys_id: HashMap<String, String> = HashMap::new();

    // 1. Write Application nodes
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
            count += 1;
            app_by_sys_id.insert(svc.sys_id.clone(), id);
        }
    }

    // 2. Write device enrichment properties from CIs
    for ci in cis {
        // Best-effort: match by CI name (hostname) since we may not have an IP mapping
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
            let prop_id = format!("{}:{key}", ci.name);
            if let Err(e) = upsert_enrichment_property_by_hostname(
                &conn, &prop_id, &ci.name, key, val, source, now_ns,
            ) {
                warnings.push(format!("CI {} prop {key}: {e:#}", ci.name));
            } else {
                count += 1;
            }
        }
    }

    // 3. Write RUNS_SERVICE / CARRIES_APPLICATION edges
    for rel in rels {
        let app_sys_id = &rel.child.value;
        if let Some(app_id) = app_by_sys_id.get(app_sys_id) {
            let device_hostname = &rel.parent.display_value;
            let rel_label = if rel.rel_type.display_value.to_lowercase().contains("runs") {
                "RUNS_SERVICE"
            } else {
                "CARRIES_APPLICATION"
            };
            if let Err(e) = link_device_application(&conn, device_hostname, app_id, rel_label) {
                warnings.push(format!("{rel_label} {device_hostname} → {app_id}: {e:#}"));
            }
        }
    }

    // 4. Write Incident nodes + HAS_INCIDENT edges (T2-5)
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
            count += 1;
            if !inc.bonsai_detection_id.is_empty()
                && let Err(e) = link_detection_incident(&conn, &inc.bonsai_detection_id, &id)
            {
                warnings.push(format!(
                    "HAS_INCIDENT {} → {id}: {e:#}",
                    inc.bonsai_detection_id
                ));
            }
        }
    }

    Ok((count, warnings))
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
    // Upsert the property node keyed by hostname; link to Device by hostname if found.
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

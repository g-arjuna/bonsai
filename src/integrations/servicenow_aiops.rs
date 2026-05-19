use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{ServiceNowAiopsConfig, ServiceNowConfig};
use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::graph::GraphStore;
use crate::graph::common::{now_ns, read_str, read_ts_ns, ts};
use crate::graph::queries;
use crate::remediation::TrustKey;
use crate::store::BonsaiStore;

const SHORT_DESC_PREFIX: &str = "[Bonsai incident ";
const PLAYBOOK_PREFIX: &str = "bonsai:playbook";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncStats {
    pub incidents_created: usize,
    pub incidents_updated: usize,
    pub incidents_resolved: usize,
    pub local_incidents_synced: usize,
    pub playbook_proposals_created: usize,
}

#[derive(Debug, Clone)]
struct DetectionSnapshot {
    id: String,
    device_address: String,
    hostname: String,
    rule_id: String,
    severity: String,
    fired_at_ns: i64,
    remediation_status: String,
    assignment_group: String,
}

#[derive(Debug, Clone)]
struct IncidentCandidate {
    id: String,
    root_detection_id: String,
    root_device_address: String,
    root_hostname: String,
    root_rule_id: String,
    severity: String,
    affected_devices: Vec<String>,
    correlated_rules: Vec<String>,
    remediation_status: String,
    assignment_group: String,
    started_at_ns: i64,
    ended_at_ns: i64,
    root_cause_hint: String,
}

#[derive(Debug, Clone)]
struct LocalIncidentRecord {
    id: String,
    snow_sys_id: String,
    state: String,
    detection_id: String,
    assignment_group: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SnowList<T> {
    result: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnowIncidentRecord {
    sys_id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    short_description: String,
    #[serde(default)]
    comments: String,
    #[serde(default)]
    work_notes: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SnowWriteResponse {
    result: SnowIncidentRecord,
}

struct SnowApi<'a> {
    http: &'a reqwest::Client,
    instance_url: &'a str,
    cfg: &'a ServiceNowAiopsConfig,
    username: &'a str,
    password: &'a str,
}

pub fn maybe_start(
    config: &ServiceNowConfig,
    store: Arc<GraphStore>,
    creds: Arc<CredentialVault>,
    mut shutdown: watch::Receiver<bool>,
) {
    if !(config.enabled && config.aiops.enabled) {
        return;
    }
    let config = config.clone();
    tokio::spawn(async move {
        info!("ServiceNow AIOps sync task started");
        let poll_secs = config.aiops.poll_interval_secs.max(30);
        let mut interval = tokio::time::interval(Duration::from_secs(poll_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = run_sync_cycle(&config, &store, &creds).await {
                        warn!("ServiceNow AIOps sync cycle failed: {e:#}");
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("ServiceNow AIOps sync task shutting down");
                        break;
                    }
                }
            }
        }
    });
}

pub async fn run_sync_cycle(
    config: &ServiceNowConfig,
    store: &Arc<GraphStore>,
    creds: &Arc<CredentialVault>,
) -> Result<SyncStats> {
    let start = std::time::Instant::now();
    let cred = creds
        .resolve(&config.credential_alias, ResolvePurpose::ServiceNowAdmin)
        .context("resolve ServiceNow admin credential for AIOps sync")?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("build ServiceNow AIOps HTTP client")?;

    let sync_inputs = load_sync_inputs(store, &config.aiops).await?;
    let api = SnowApi {
        http: &http,
        instance_url: config.instance_url.trim_end_matches('/'),
        cfg: &config.aiops,
        username: &cred.username,
        password: &cred.password,
    };
    let mut remote_by_id = fetch_remote_incidents(&api).await?;

    let mut stats = SyncStats::default();
    let mut active_ids = HashSet::new();

    for candidate in &sync_inputs.active_incidents {
        active_ids.insert(candidate.id.clone());
        let existing_local = sync_inputs.local_incidents.get(&candidate.id);
        let existing_remote = existing_local
            .and_then(|local| remote_by_id.get(&local.id).cloned())
            .or_else(|| remote_by_id.get(&candidate.id).cloned());
        let snow =
            upsert_remote_incident(&api, candidate, existing_remote.as_ref(), &mut stats).await?;
        upsert_local_incident(
            store,
            &candidate.id,
            &snow.sys_id,
            &config.aiops.open_state,
            &candidate.assignment_group,
            candidate.started_at_ns,
            &candidate.root_detection_id,
        )
        .await?;
        stats.local_incidents_synced += 1;

        // Change-context back-annotation: if the device is in an active change
        // window, link the incident to the ChangeRequest so operators see the
        // CHG reference on the ServiceNow incident.
        if let Ok(ctx) = crate::integrations::change_management::active_change_context(
            store,
            &candidate.root_device_address,
        )
        .await
        {
            for change in &ctx.change_requests {
                let _ = crate::integrations::change_management::link_incident_to_change(
                    store,
                    &candidate.id,
                    &change.id,
                )
                .await;
                // Best-effort: annotate the SNOW incident with the CHG number.
                let _ = annotate_snow_incident_with_change(
                    &api,
                    &snow.sys_id,
                    &change.number,
                    &change.short_description,
                )
                .await;
            }
        }
        remote_by_id.insert(candidate.id.clone(), snow);
    }

    if config.aiops.auto_clear {
        for local in sync_inputs.local_incidents.values() {
            let already_resolved = local.state == config.aiops.resolved_state;
            if active_ids.contains(&local.id) || already_resolved {
                continue;
            }
            if let Some(remote) = remote_by_id.get(&local.id) {
                resolve_remote_incident(&api, remote).await?;
                stats.incidents_resolved += 1;
            }
            upsert_local_incident(
                store,
                &local.id,
                &local.snow_sys_id,
                &config.aiops.resolved_state,
                &local.assignment_group,
                now_ns(),
                &local.detection_id,
            )
            .await?;
            stats.local_incidents_synced += 1;
        }
    }

    if config.aiops.playbook_bridge_enabled {
        for remote in remote_by_id.values() {
            let incident_id = parse_incident_id(&remote.short_description).unwrap_or_default();
            if incident_id.is_empty() {
                continue;
            }
            let detection_id = sync_inputs
                .local_incidents
                .get(&incident_id)
                .map(|r| r.detection_id.clone())
                .or_else(|| {
                    sync_inputs
                        .active_incidents
                        .iter()
                        .find(|candidate| candidate.id == incident_id)
                        .map(|candidate| candidate.root_detection_id.clone())
                });
            let Some(detection_id) = detection_id else {
                continue;
            };
            for playbook_id in parse_playbook_commands(&remote.comments, &remote.work_notes) {
                if ensure_playbook_proposal(store, &detection_id, &playbook_id).await? {
                    acknowledge_playbook_bridge(&api, &remote.sys_id, &playbook_id).await?;
                    stats.playbook_proposals_created += 1;
                }
            }
        }
    }

    let duration = start.elapsed().as_secs_f64();
    metrics::histogram!("bonsai_servicenow_sync_duration_seconds", duration, "sync_type" => "aiops");

    Ok(stats)
}

struct SyncInputs {
    active_incidents: Vec<IncidentCandidate>,
    local_incidents: HashMap<String, LocalIncidentRecord>,
}

async fn load_sync_inputs(
    store: &Arc<GraphStore>,
    cfg: &ServiceNowAiopsConfig,
) -> Result<SyncInputs> {
    let db = store.db();
    let active_window_ns = cfg.active_window_secs as i64 * 1_000_000_000;
    let correlation_window_secs = cfg.correlation_window_secs;
    let max_hops = cfg.max_blast_radius_hops;
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("open graph for ServiceNow AIOps sync")?;
        let detections = read_detection_snapshots(&conn, now_ns() - active_window_ns)?;
        let degree_map = read_degree_map(&conn)?;
        let candidates = group_detections(
            detections,
            correlation_window_secs,
            &degree_map,
            |device_address| {
                let blast = queries::blast_radius(&conn, device_address, max_hops)?;
                Ok::<String, anyhow::Error>(format_root_cause_hint(&blast))
            },
        )?;
        let local_incidents = read_local_incidents(&conn)?;
        Ok::<SyncInputs, anyhow::Error>(SyncInputs {
            active_incidents: candidates,
            local_incidents,
        })
    })
    .await
    .context("spawn_blocking panicked")?
}

fn read_detection_snapshots(
    conn: &Connection<'_>,
    cutoff_ns: i64,
) -> Result<Vec<DetectionSnapshot>> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device)-[:TRIGGERED]->(e:DetectionEvent) \
             OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
             WHERE e.fired_at > $cutoff \
             RETURN e.id, d.address, d.hostname, e.rule_id, e.severity, e.fired_at, \
                    coalesce(r.status, ''), coalesce(d.snow_assignment_group, '') \
             ORDER BY e.fired_at DESC \
             LIMIT 500",
        )
        .context("prepare ServiceNow AIOps detection query")?;
    let rows = conn
        .execute(&mut stmt, vec![("cutoff", ts(cutoff_ns))])
        .context("execute ServiceNow AIOps detection query")?;
    Ok(rows
        .map(|row| DetectionSnapshot {
            id: read_str(&row[0]),
            device_address: read_str(&row[1]),
            hostname: read_str(&row[2]),
            rule_id: read_str(&row[3]),
            severity: read_str(&row[4]),
            fired_at_ns: read_ts_ns(&row[5]),
            remediation_status: read_str(&row[6]),
            assignment_group: read_str(&row[7]),
        })
        .collect())
}

fn read_degree_map(conn: &Connection<'_>) -> Result<HashMap<String, usize>> {
    Ok(conn
        .query(
            "MATCH (a:Interface)-[:CONNECTED_TO]->(:Interface) \
             RETURN a.device_address",
        )
        .context("read ServiceNow AIOps degree map")?
        .fold(HashMap::new(), |mut map, row| {
            *map.entry(read_str(&row[0])).or_insert(0usize) += 1;
            map
        }))
}

fn read_local_incidents(conn: &Connection<'_>) -> Result<HashMap<String, LocalIncidentRecord>> {
    Ok(conn
        .query(
            "MATCH (i:Incident) RETURN i.id, i.snow_sys_id, i.state, i.detection_id, i.assignment_group",
        )
        .context("read local incidents")?
        .map(|row| {
            let record = LocalIncidentRecord {
                id: read_str(&row[0]),
                snow_sys_id: read_str(&row[1]),
                state: read_str(&row[2]),
                detection_id: read_str(&row[3]),
                assignment_group: read_str(&row[4]),
            };
            (record.id.clone(), record)
        })
        .collect())
}

fn group_detections<F>(
    mut detections: Vec<DetectionSnapshot>,
    window_secs: u64,
    degree_map: &HashMap<String, usize>,
    mut root_cause_hint: F,
) -> Result<Vec<IncidentCandidate>>
where
    F: FnMut(&str) -> Result<String>,
{
    detections.sort_by_key(|d| d.fired_at_ns);
    let window_ns = (window_secs as i64).saturating_mul(1_000_000_000);
    let mut groups: Vec<Vec<DetectionSnapshot>> = Vec::new();
    for det in detections {
        if let Some(group) = groups
            .iter_mut()
            .rev()
            .find(|group| det.fired_at_ns - group[0].fired_at_ns <= window_ns)
        {
            group.push(det);
        } else {
            groups.push(vec![det]);
        }
    }

    let mut out = Vec::new();
    for mut group in groups {
        group.sort_by_key(|d| d.fired_at_ns);
        let root_idx = group
            .iter()
            .enumerate()
            .max_by_key(|(_, d)| {
                (
                    *degree_map.get(&d.device_address).unwrap_or(&0usize),
                    -d.fired_at_ns,
                )
            })
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let root = group.remove(root_idx);
        let affected_devices = sorted_unique(
            std::iter::once(root.device_address.clone())
                .chain(group.iter().map(|d| d.device_address.clone())),
        );
        let correlated_rules = sorted_unique(
            std::iter::once(root.rule_id.clone()).chain(group.iter().map(|d| d.rule_id.clone())),
        );
        let severity = max_severity(
            std::iter::once(root.severity.as_str())
                .chain(group.iter().map(|d| d.severity.as_str())),
        );
        let remediation_status = std::iter::once(&root)
            .chain(group.iter())
            .find(|d| !d.remediation_status.is_empty())
            .map(|d| d.remediation_status.clone())
            .unwrap_or_else(|| "none".to_string());
        let assignment_group = if !root.assignment_group.trim().is_empty() {
            root.assignment_group.clone()
        } else {
            String::new()
        };
        out.push(IncidentCandidate {
            id: root.id.clone(),
            root_detection_id: root.id.clone(),
            root_device_address: root.device_address.clone(),
            root_hostname: root.hostname.clone(),
            root_rule_id: root.rule_id.clone(),
            severity,
            affected_devices,
            correlated_rules,
            remediation_status,
            assignment_group,
            started_at_ns: root.fired_at_ns,
            ended_at_ns: group
                .last()
                .map(|d| d.fired_at_ns)
                .unwrap_or(root.fired_at_ns),
            root_cause_hint: root_cause_hint(&root.device_address)?,
        });
    }

    Ok(out)
}

fn sorted_unique<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut values: Vec<String> = values
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    values.sort();
    values
}

fn max_severity<'a, I>(values: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let rank = |severity: &str| match severity {
        "critical" => 3,
        "high" => 2,
        "warning" | "warn" => 1,
        _ => 0,
    };
    values
        .into_iter()
        .max_by_key(|severity| rank(severity))
        .unwrap_or("info")
        .to_string()
}

fn format_root_cause_hint(blast: &queries::BlastRadiusResult) -> String {
    let mut parts = Vec::new();
    if !blast.reachable_devices.is_empty() {
        parts.push(format!(
            "{} reachable devices",
            blast.reachable_devices.len()
        ));
    }
    if !blast.direct_apps.is_empty() || !blast.neighbor_apps.is_empty() {
        parts.push(format!(
            "applications in blast radius: {} direct / {} adjacent",
            blast.direct_apps.len(),
            blast.neighbor_apps.len()
        ));
    }
    if !blast.active_detections.is_empty() {
        parts.push(format!(
            "related detections: {}",
            blast.active_detections.join(", ")
        ));
    }
    if parts.is_empty() {
        "No additional blast-radius context available yet.".to_string()
    } else {
        parts.join("; ")
    }
}

async fn fetch_remote_incidents(api: &SnowApi<'_>) -> Result<HashMap<String, SnowIncidentRecord>> {
    let mut url = Url::parse(&format!(
        "{}/api/now/table/{}",
        api.instance_url, api.cfg.incident_table
    ))
    .context("build ServiceNow incident list URL")?;
    url.query_pairs_mut()
        .append_pair(
            "sysparm_query",
            &format!("short_descriptionSTARTSWITH{}", SHORT_DESC_PREFIX),
        )
        .append_pair(
            "sysparm_fields",
            "sys_id,number,state,short_description,comments,work_notes,assignment_group",
        )
        .append_pair("sysparm_display_value", "true")
        .append_pair("sysparm_limit", "200");
    let list: SnowList<SnowIncidentRecord> = api
        .http
        .get(url)
        .basic_auth(api.username, Some(api.password))
        .send()
        .await
        .context("GET ServiceNow incident list")?
        .error_for_status()
        .context("ServiceNow incident list returned error")?
        .json()
        .await
        .context("parse ServiceNow incident list")?;
    Ok(list
        .result
        .into_iter()
        .filter_map(|record| parse_incident_id(&record.short_description).map(|id| (id, record)))
        .collect())
}

async fn upsert_remote_incident(
    api: &SnowApi<'_>,
    candidate: &IncidentCandidate,
    existing: Option<&SnowIncidentRecord>,
    stats: &mut SyncStats,
) -> Result<SnowIncidentRecord> {
    let mut desired = incident_payload(candidate, api.cfg);
    if !candidate.assignment_group.trim().is_empty() {
        desired.insert(
            "assignment_group".to_string(),
            json!(candidate.assignment_group.clone()),
        );
    } else if !api.cfg.assignment_group_fallback.trim().is_empty() {
        desired.insert(
            "assignment_group".to_string(),
            json!(api.cfg.assignment_group_fallback.clone()),
        );
    }

    if let Some(existing) = existing {
        if existing.state == api.cfg.resolved_state {
            desired.insert("state".to_string(), json!(api.cfg.open_state.clone()));
        }
        let url = format!(
            "{}/api/now/table/{}/{}",
            api.instance_url, api.cfg.incident_table, existing.sys_id
        );
        let response = post_or_patch_with_assignment_fallback(
            api.http,
            reqwest::Method::PATCH,
            &url,
            api.username,
            api.password,
            desired,
        )
        .await?;
        stats.incidents_updated += 1;
        return Ok(response.result);
    }

    let url = format!(
        "{}/api/now/table/{}",
        api.instance_url, api.cfg.incident_table
    );
    let response = post_or_patch_with_assignment_fallback(
        api.http,
        reqwest::Method::POST,
        &url,
        api.username,
        api.password,
        desired,
    )
    .await?;
    stats.incidents_created += 1;
    Ok(response.result)
}

async fn resolve_remote_incident(api: &SnowApi<'_>, remote: &SnowIncidentRecord) -> Result<()> {
    let url = format!(
        "{}/api/now/table/{}/{}",
        api.instance_url, api.cfg.incident_table, remote.sys_id
    );
    api.http
        .patch(&url)
        .basic_auth(api.username, Some(api.password))
        .json(&json!({
            "state": api.cfg.resolved_state,
            "close_notes": "Bonsai auto-cleared this incident after detections went quiet.",
        }))
        .send()
        .await
        .with_context(|| format!("PATCH {url}"))?
        .error_for_status()
        .context("ServiceNow incident resolve returned error")?;
    Ok(())
}

async fn acknowledge_playbook_bridge(
    api: &SnowApi<'_>,
    sys_id: &str,
    playbook_id: &str,
) -> Result<()> {
    let url = format!(
        "{}/api/now/table/{}/{}",
        api.instance_url, api.cfg.incident_table, sys_id
    );
    api.http
        .patch(&url)
        .basic_auth(api.username, Some(api.password))
        .json(&json!({
            "work_notes": format!("Bonsai queued remediation proposal for playbook `{playbook_id}`."),
        }))
        .send()
        .await
        .with_context(|| format!("PATCH {url}"))?
        .error_for_status()
        .context("ServiceNow bridge acknowledgement returned error")?;
    Ok(())
}

fn incident_payload(
    candidate: &IncidentCandidate,
    cfg: &ServiceNowAiopsConfig,
) -> serde_json::Map<String, serde_json::Value> {
    let node = if candidate.root_hostname.trim().is_empty() {
        candidate.root_device_address.clone()
    } else {
        candidate.root_hostname.clone()
    };
    serde_json::Map::from_iter([
        (
            "short_description".to_string(),
            json!(format!(
                "{SHORT_DESC_PREFIX}{}] {} {} on {}",
                candidate.id, candidate.severity, candidate.root_rule_id, node
            )),
        ),
        ("state".to_string(), json!(cfg.open_state)),
        ("category".to_string(), json!("network")),
        ("subcategory".to_string(), json!("aiops")),
        (
            "impact".to_string(),
            json!(severity_to_impact(&candidate.severity)),
        ),
        (
            "urgency".to_string(),
            json!(severity_to_urgency(&candidate.severity)),
        ),
        (
            "description".to_string(),
            json!(build_incident_description(candidate)),
        ),
    ])
}

fn build_incident_description(candidate: &IncidentCandidate) -> String {
    format!(
        "Bonsai correlated detection `{}` as the root cause.\n\
Affected devices: {}\n\
Correlated rules: {}\n\
Remediation status: {}\n\
Incident window: {} → {}\n\
Blast radius hint: {}\n\
To request a Bonsai playbook proposal from ServiceNow, add a comment or work note with `{PLAYBOOK_PREFIX} <playbook_id>`.",
        candidate.root_rule_id,
        candidate.affected_devices.join(", "),
        candidate.correlated_rules.join(", "),
        candidate.remediation_status,
        candidate.started_at_ns,
        candidate.ended_at_ns,
        candidate.root_cause_hint,
    )
}

fn severity_to_impact(severity: &str) -> &'static str {
    match severity {
        "critical" => "1",
        "high" => "2",
        "warning" | "warn" => "2",
        _ => "3",
    }
}

fn severity_to_urgency(severity: &str) -> &'static str {
    match severity {
        "critical" => "1",
        "high" => "1",
        "warning" | "warn" => "2",
        _ => "3",
    }
}

async fn post_or_patch_with_assignment_fallback(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    username: &str,
    password: &str,
    mut payload: serde_json::Map<String, serde_json::Value>,
) -> Result<SnowWriteResponse> {
    let attempt = request_json(http, method.clone(), url, username, password, &payload).await;
    match attempt {
        Ok(response) => Ok(response),
        Err(err) if payload.remove("assignment_group").is_some() => {
            warn!("ServiceNow rejected assignment_group, retrying without it: {err:#}");
            request_json(http, method, url, username, password, &payload).await
        }
        Err(err) => Err(err),
    }
}

async fn request_json(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    username: &str,
    password: &str,
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<SnowWriteResponse> {
    let req = match method {
        reqwest::Method::POST => http.post(url),
        reqwest::Method::PATCH => http.patch(url),
        _ => anyhow::bail!("unsupported method {method}"),
    };
    req.basic_auth(username, Some(password))
        .json(payload)
        .send()
        .await
        .with_context(|| format!("{} {}", method, url))?
        .error_for_status()
        .with_context(|| format!("{} {} returned error", method, url))?
        .json()
        .await
        .context("parse ServiceNow write response")
}

/// Annotate a ServiceNow incident with a related change ticket reference.
async fn annotate_snow_incident_with_change(
    api: &SnowApi<'_>,
    incident_sys_id: &str,
    change_number: &str,
    change_description: &str,
) -> Result<()> {
    let url = format!(
        "{}/api/now/table/{}/{}",
        api.instance_url, api.cfg.incident_table, incident_sys_id
    );
    api.http
        .patch(&url)
        .basic_auth(api.username, Some(api.password))
        .json(&json!({
            "work_notes": format!(
                "Bonsai: This incident occurred during active change {change_number} ({change_description}). \
                 The detection may be expected maintenance noise."
            ),
        }))
        .send()
        .await
        .with_context(|| format!("PATCH {url}"))?
        .error_for_status()
        .context("annotate incident with change reference returned error")?;
    Ok(())
}

async fn upsert_local_incident(
    store: &Arc<GraphStore>,
    id: &str,
    snow_sys_id: &str,
    state: &str,
    assignment_group: &str,
    opened_at_ns: i64,
    detection_id: &str,
) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let id = id.to_string();
    let snow_sys_id = snow_sys_id.to_string();
    let state = state.to_string();
    let assignment_group = assignment_group.to_string();
    let detection_id = detection_id.to_string();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db).context("local incident upsert connection")?;
        let mut stmt = conn
            .prepare(
                "MERGE (i:Incident {id: $id}) \
                 SET i.snow_sys_id = $sid, i.state = $state, i.assignment_group = $ag, \
                     i.opened_at_ns = $opened_at, i.detection_id = $did, i.updated_at = $updated_at",
            )
            .context("prepare local incident upsert")?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id.clone())),
                ("sid", Value::String(snow_sys_id)),
                ("state", Value::String(state)),
                ("ag", Value::String(assignment_group)),
                ("opened_at", Value::Int64(opened_at_ns)),
                ("did", Value::String(detection_id.clone())),
                ("updated_at", ts(now_ns())),
            ],
        )
        .context("execute local incident upsert")?;
        let mut edge = conn
            .prepare(
                "MATCH (e:DetectionEvent {id: $eid}), (i:Incident {id: $iid}) \
                 MERGE (e)-[:HAS_INCIDENT]->(i)",
            )
            .context("prepare local incident edge")?;
        conn.execute(
            &mut edge,
            vec![
                ("eid", Value::String(detection_id)),
                ("iid", Value::String(id)),
            ],
        )
        .context("execute local incident edge")?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("spawn_blocking panicked")?
}

async fn ensure_playbook_proposal(
    store: &Arc<GraphStore>,
    detection_id: &str,
    playbook_id: &str,
) -> Result<bool> {
    let db = store.db();
    let detection_id_string = detection_id.to_string();
    let playbook_id_string = playbook_id.to_string();
    let existing = tokio::task::spawn_blocking({
        let db = Arc::clone(&db);
        let detection_id = detection_id_string.clone();
        let playbook_id = playbook_id_string.clone();
        move || -> Result<bool> {
            let conn = Connection::new(&db).context("playbook bridge read connection")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (p:RemediationProposal) \
                     WHERE p.detection_id = $did AND p.playbook_id = $pid \
                     RETURN count(*)",
                )
                .context("prepare playbook bridge duplicate check")?;
            let count = conn
                .execute(
                    &mut stmt,
                    vec![
                        ("did", Value::String(detection_id)),
                        ("pid", Value::String(playbook_id)),
                    ],
                )
                .context("execute playbook bridge duplicate check")?
                .next()
                .map(|row| match &row[0] {
                    Value::Int64(v) => *v,
                    Value::Int32(v) => *v as i64,
                    _ => 0,
                })
                .unwrap_or(0);
            Ok(count > 0)
        }
    })
    .await
    .context("spawn_blocking panicked")??;
    if existing {
        return Ok(false);
    }

    let (rule_id, env_name, site_name) = load_detection_context(store, detection_id).await?;
    let trust_key = TrustKey::new(&rule_id, &env_name, &site_name, playbook_id).to_storage_key();
    store
        .write_remediation_proposal(
            detection_id.to_string(),
            playbook_id.to_string(),
            trust_key,
            String::new(),
            String::new(),
            now_ns(),
        )
        .await?;
    Ok(true)
}

async fn load_detection_context(
    store: &Arc<GraphStore>,
    detection_id: &str,
) -> Result<(String, String, String)> {
    let db = store.db();
    let detection_id = detection_id.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("detection context connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device)-[:TRIGGERED]->(e:DetectionEvent {id: $id}) \
                 OPTIONAL MATCH (d)-[:LOCATED_AT]->(s:Site)-[:BELONGS_TO_ENVIRONMENT]->(env:Environment) \
                 RETURN e.rule_id, coalesce(env.name, ''), coalesce(s.name, '')",
            )
            .context("prepare detection context query")?;
        let row = conn
            .execute(&mut stmt, vec![("id", Value::String(detection_id))])
            .context("execute detection context query")?
            .next()
            .context("missing detection context row")?;
        Ok::<(String, String, String), anyhow::Error>((
            read_str(&row[0]),
            if read_str(&row[1]).is_empty() {
                "home_lab".to_string()
            } else {
                read_str(&row[1])
            },
            if read_str(&row[2]).is_empty() {
                "unknown-site".to_string()
            } else {
                read_str(&row[2])
            },
        ))
    })
    .await
    .context("spawn_blocking panicked")?
}

fn parse_incident_id(short_description: &str) -> Option<String> {
    let rest = short_description.strip_prefix(SHORT_DESC_PREFIX)?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

fn parse_playbook_commands(comments: &str, work_notes: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in comments.lines().chain(work_notes.lines()) {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix(PLAYBOOK_PREFIX) {
            let original = &trimmed[trimmed.len() - rest.len()..];
            let playbook = original
                .trim_start_matches([':', ' '])
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim();
            if !playbook.is_empty() {
                out.push(playbook.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playbook_commands_are_parsed_from_comments_and_notes() {
        let cmds = parse_playbook_commands(
            "please review\nbonsai:playbook bgp_restart\nignored",
            "Bonsai:playbook   interface_bounce\nbonsai:playbook bgp_restart",
        );
        assert_eq!(
            cmds,
            vec!["bgp_restart".to_string(), "interface_bounce".to_string()]
        );
    }

    #[test]
    fn incident_id_round_trips_from_short_description() {
        let id = parse_incident_id("[Bonsai incident det-123] critical bgp on leaf1");
        assert_eq!(id.as_deref(), Some("det-123"));
    }

    #[test]
    fn incident_description_contains_operator_bridge_hint() {
        let candidate = IncidentCandidate {
            id: "det-1".to_string(),
            root_detection_id: "det-1".to_string(),
            root_device_address: "10.0.0.1".to_string(),
            root_hostname: "leaf1".to_string(),
            root_rule_id: "bgp_session_down".to_string(),
            severity: "critical".to_string(),
            affected_devices: vec!["10.0.0.1".to_string(), "10.0.0.2".to_string()],
            correlated_rules: vec![
                "bgp_session_down".to_string(),
                "bfd_session_down".to_string(),
            ],
            remediation_status: "none".to_string(),
            assignment_group: String::new(),
            started_at_ns: 1,
            ended_at_ns: 2,
            root_cause_hint: "2 reachable devices".to_string(),
        };
        let description = build_incident_description(&candidate);
        assert!(description.contains("bgp_session_down"));
        assert!(description.contains("bonsai:playbook <playbook_id>"));
        assert!(description.contains("2 reachable devices"));
    }

    #[test]
    fn root_cause_hint_formats_available_context() {
        let blast = queries::BlastRadiusResult {
            origin_address: "10.0.0.1".to_string(),
            origin_hostname: "leaf1".to_string(),
            site_name: "dc1".to_string(),
            env_name: "data_center".to_string(),
            reachable_devices: vec![queries::DeviceRef {
                address: "10.0.0.2".to_string(),
                hostname: "spine1".to_string(),
                vendor: "nokia".to_string(),
            }],
            direct_apps: vec!["svc-a".to_string()],
            neighbor_apps: vec!["svc-b".to_string()],
            active_detections: vec!["10.0.0.2:bgp_session_down".to_string()],
        };
        let hint = format_root_cause_hint(&blast);
        assert!(hint.contains("1 reachable devices"));
        assert!(hint.contains("1 direct / 1 adjacent"));
        assert!(hint.contains("bgp_session_down"));
    }
}

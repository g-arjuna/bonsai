use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::api::pb::{AssignmentUpdate, DeviceAssignment};
use crate::config::{AssignmentRule, TargetConfig};
use crate::credentials::CredentialVault;
use crate::registry::{ApiRegistry, DeviceRegistry, RegistryChange};

pub struct CollectorManager {
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    active_collectors: Arc<Mutex<HashMap<String, mpsc::Sender<AssignmentUpdate>>>>,
    runtime_state: Arc<Mutex<HashMap<String, CollectorRuntimeState>>>,
    /// Routing rules sorted descending by priority (highest first).
    rules: Arc<Mutex<Vec<AssignmentRule>>>,
}

#[derive(Clone, Default)]
struct CollectorRuntimeState {
    connected: bool,
    queue_depth_updates: u64,
    subscription_count: u32,
    uptime_secs: i64,
    last_heartbeat_ns: i64,
}

impl CollectorManager {
    pub fn new(
        registry: Arc<ApiRegistry>,
        credentials: Arc<CredentialVault>,
        initial_rules: Vec<AssignmentRule>,
    ) -> Self {
        let mut rules = initial_rules;
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        let manager = Self {
            registry,
            credentials,
            active_collectors: Arc::new(Mutex::new(HashMap::new())),
            runtime_state: Arc::new(Mutex::new(HashMap::new())),
            rules: Arc::new(Mutex::new(rules)),
        };
        manager.start_registry_watcher();
        manager
    }

    /// Returns the current routing rules (sorted by priority descending).
    pub fn get_rules(&self) -> Vec<AssignmentRule> {
        self.rules.lock().expect("rules lock poisoned").clone()
    }

    /// Replaces all routing rules and re-evaluates unassigned devices.
    pub fn set_rules(&self, new_rules: Vec<AssignmentRule>) {
        let mut sorted = new_rules;
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));
        *self.rules.lock().expect("rules lock poisoned") = sorted;
        info!("assignment rules updated; re-evaluating unassigned devices");
        self.reassign_unassigned();
    }

    /// Evaluates routing rules against a target. Returns the matched collector_id or None.
    pub fn assign_by_rules(&self, target: &TargetConfig) -> Option<String> {
        let rules = self.rules.lock().expect("rules lock poisoned");
        let device_site = target.site.as_deref().unwrap_or("");
        let device_role = target.role.as_deref().unwrap_or("");
        for rule in rules.iter() {
            if rule.match_site != device_site {
                continue;
            }
            if let Some(ref required_role) = rule.match_role {
                if required_role != device_role {
                    continue;
                }
            }
            return Some(rule.collector_id.clone());
        }
        None
    }

    /// Re-evaluates all unassigned devices and assigns them if a rule matches.
    pub fn reassign_unassigned(&self) {
        let all_targets = match self.registry.list_all_targets() {
            Ok(t) => t,
            Err(e) => {
                warn!(%e, "failed to list targets for rule re-evaluation");
                return;
            }
        };
        for target in all_targets {
            if target.collector_id.is_some() {
                continue;
            }
            if let Some(collector_id) = self.assign_by_rules(&target) {
                info!(
                    address = %target.address,
                    %collector_id,
                    "auto-assigning device via routing rule"
                );
                if let Err(e) = self.registry.assign_device_with_audit(
                    &target.address,
                    Some(collector_id),
                    "system",
                    "assignment_rule_auto_assign",
                ) {
                    warn!(address = %target.address, %e, "failed to auto-assign device");
                }
            }
        }
    }

    fn start_registry_watcher(&self) {
        let registry = self.registry.clone();
        let credentials = self.credentials.clone();
        let active_collectors = self.active_collectors.clone();
        let rules = self.rules.clone();

        tokio::spawn(async move {
            let mut changes = registry.subscribe_changes();
            while let Some(change) = changes.recv().await {
                match change {
                    RegistryChange::Added(mut target) | RegistryChange::Updated(mut target) => {
                        // Auto-assign if no explicit collector_id and rules match.
                        if target.collector_id.is_none() {
                            if let Some(collector_id) = find_collector_by_rules(&target, &rules) {
                                info!(
                                    address = %target.address,
                                    %collector_id,
                                    "auto-assigning device via routing rule"
                                );
                                match registry.assign_device_with_audit(
                                    &target.address,
                                    Some(collector_id.clone()),
                                    "system",
                                    "assignment_rule_auto_assign",
                                ) {
                                    Ok(updated) => target = updated,
                                    Err(e) => warn!(address = %target.address, %e, "failed to auto-assign device"),
                                }
                            }
                        }

                        if let Some(collector_id) = &target.collector_id {
                            let tx = {
                                let collectors = active_collectors.lock().unwrap();
                                collectors.get(collector_id.as_str()).cloned()
                            };
                            if let Some(tx) = tx {
                                let assignment = create_assignment(&target, &credentials);
                                let update = AssignmentUpdate {
                                    assignments: vec![assignment],
                                    is_full_sync: false,
                                };
                                let _ = tx.send(update).await;
                            }
                        }
                    }
                    RegistryChange::Removed(address) => {
                        // Nothing to push — collector will receive a full sync on re-registration.
                        info!(%address, "device removed from registry");
                    }
                }
            }
        });
    }

    pub async fn register_collector(
        &self,
        collector_id: String,
    ) -> Result<mpsc::Receiver<AssignmentUpdate>> {
        let (tx, rx) = mpsc::channel(32);

        let targets = self.registry.list_assigned_to(&collector_id)?;
        let mut assignments = Vec::new();
        for target in targets {
            assignments.push(create_assignment(&target, &self.credentials));
        }

        tx.send(AssignmentUpdate {
            assignments,
            is_full_sync: true,
        })
        .await?;

        {
            let mut collectors = self.active_collectors.lock().unwrap();
            collectors.insert(collector_id.clone(), tx);
        }
        {
            let mut runtime = self.runtime_state.lock().unwrap();
            let entry = runtime.entry(collector_id.clone()).or_default();
            entry.connected = true;
            entry.last_heartbeat_ns = now_ns();
        }

        info!(%collector_id, "collector registered, initial assignments sent");
        Ok(rx)
    }

    pub fn unregister_collector(&self, collector_id: &str) {
        {
            let mut collectors = self.active_collectors.lock().unwrap();
            collectors.remove(collector_id);
        }
        {
            let mut runtime = self.runtime_state.lock().unwrap();
            if let Some(entry) = runtime.get_mut(collector_id) {
                entry.connected = false;
            }
        }
        info!(%collector_id, "collector unregistered; re-evaluating its devices");

        // Clear collector_id on all devices this collector owned, then re-evaluate
        // via rules so they land on another collector if one matches.
        let owned = self
            .registry
            .list_assigned_to(collector_id)
            .unwrap_or_default();

        for target in owned {
            // Clear assignment first.
            if let Err(e) = self.registry.assign_device_with_audit(
                &target.address,
                None,
                "system",
                "collector_unregister_clear_assignment",
            ) {
                warn!(address = %target.address, %e, "failed to clear collector assignment");
                continue;
            }
            // Try to find a new collector via rules.
            if let Some(new_collector) = self.assign_by_rules(&target) {
                info!(
                    address = %target.address,
                    %new_collector,
                    "re-assigning orphaned device to new collector"
                );
                if let Err(e) = self.registry.assign_device_with_audit(
                    &target.address,
                    Some(new_collector),
                    "system",
                    "collector_unregister_reassign",
                ) {
                    warn!(address = %target.address, %e, "failed to re-assign orphaned device");
                }
            } else {
                warn!(
                    address = %target.address,
                    old_collector = %collector_id,
                    "device is now unassigned — no routing rule matches"
                );
            }
        }
    }

    pub fn record_heartbeat(
        &self,
        collector_id: &str,
        queue_depth_updates: u64,
        subscription_count: u32,
        uptime_secs: i64,
    ) {
        let mut runtime = self.runtime_state.lock().unwrap();
        let entry = runtime.entry(collector_id.to_string()).or_default();
        entry.connected = true;
        entry.queue_depth_updates = queue_depth_updates;
        entry.subscription_count = subscription_count;
        entry.uptime_secs = uptime_secs;
        entry.last_heartbeat_ns = now_ns();
    }

    /// Lists all devices and their assignment status for the UI.
    pub fn collector_status_summary(&self) -> CollectorStatusSummary {
        let connected_ids: std::collections::HashSet<String> = self
            .active_collectors
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let runtime = self.runtime_state.lock().unwrap().clone();

        let all_targets = self.registry.list_all_targets().unwrap_or_default();

        let mut by_collector: HashMap<String, Vec<String>> = HashMap::new();
        let mut unassigned_devices = Vec::new();

        for target in &all_targets {
            match &target.collector_id {
                Some(col_id) => by_collector.entry(col_id.clone()).or_default().push(target.address.clone()),
                None => unassigned_devices.push(target.address.clone()),
            }
        }

        // Connected collectors first, then known-but-disconnected ones.
        let mut all_ids: std::collections::HashSet<String> = connected_ids.iter().cloned().collect();
        all_ids.extend(by_collector.keys().cloned());
        all_ids.extend(runtime.keys().cloned());

        let mut collectors: Vec<CollectorStatus> = all_ids
            .into_iter()
            .map(|id| {
                let assigned = by_collector.get(&id).cloned().unwrap_or_default();
                let runtime_state = runtime.get(&id).cloned().unwrap_or_default();
                let connected = connected_ids.contains(&id) || runtime_state.connected;
                CollectorStatus {
                    id,
                    connected,
                    assigned_device_count: assigned.len(),
                    assigned_targets: assigned,
                    queue_depth_updates: runtime_state.queue_depth_updates,
                    subscription_count: runtime_state.subscription_count,
                    uptime_secs: runtime_state.uptime_secs,
                    last_heartbeat_ns: runtime_state.last_heartbeat_ns,
                }
            })
            .collect();
        collectors.sort_by(|a, b| a.id.cmp(&b.id));

        CollectorStatusSummary { collectors, unassigned_devices }
    }
}

pub struct CollectorStatus {
    pub id: String,
    pub connected: bool,
    pub assigned_device_count: usize,
    pub assigned_targets: Vec<String>,
    pub queue_depth_updates: u64,
    pub subscription_count: u32,
    pub uptime_secs: i64,
    pub last_heartbeat_ns: i64,
}

pub struct CollectorStatusSummary {
    pub collectors: Vec<CollectorStatus>,
    pub unassigned_devices: Vec<String>,
}

fn find_collector_by_rules(
    target: &TargetConfig,
    rules: &Mutex<Vec<AssignmentRule>>,
) -> Option<String> {
    let rules = rules.lock().expect("rules lock poisoned");
    let device_site = target.site.as_deref().unwrap_or("");
    let device_role = target.role.as_deref().unwrap_or("");
    for rule in rules.iter() {
        if rule.match_site != device_site {
            continue;
        }
        if let Some(ref required_role) = rule.match_role {
            if required_role != device_role {
                continue;
            }
        }
        return Some(rule.collector_id.clone());
    }
    None
}

fn create_assignment(target: &TargetConfig, vault: &CredentialVault) -> DeviceAssignment {
    let mut username = String::new();
    let mut password = String::new();

    if let Some(alias) = &target.credential_alias {
        if let Ok(creds) = vault.resolve(alias) {
            username = creds.username;
            password = creds.password;
        }
    } else if let Some(u) = target.resolved_username() {
        username = u;
        password = target.resolved_password().unwrap_or_default();
    }

    DeviceAssignment {
        device: Some(crate::api::managed_device_from_target(target)),
        username,
        password,
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

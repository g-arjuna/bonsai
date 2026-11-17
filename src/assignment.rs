use anyhow::Result;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::api::pb::{AssignmentUpdate, DeviceAssignment};
use crate::api::{
    ACQUISITION_CONTRACT_VERSION, ASSIGNMENT_LIFECYCLE_CONTRACT_VERSION, SECURITY_CONTRACT_VERSION,
};
use crate::config::{AcquisitionMode, AssignmentRule, TargetConfig};
use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::graph::{BonsaiEvent, SiteRecord};
use crate::registry::ApiRegistry;

/// Settings-table key for the persisted assignment-authority epoch
/// high-water mark (M5.2B restart/clock-rollback fencing).
const ASSIGNMENT_EPOCH_HW_KEY: &str = "assignment.authority_epoch_high_water";

/// One live assignment stream session. `generation` fences reconnect
/// races: an old stream closing must never unregister a newer session
/// registered under the same collector id (BN-021g G4).
struct CollectorSession {
    generation: u64,
    /// Collector-advertised protocol version from CollectorIdentity. Sessions
    /// below ACQUISITION_CONTRACT_VERSION cannot enforce explicit/disabled
    /// acquisition intent and must never receive those assignments.
    protocol_version: u32,
    /// Whether this session may receive credential-bearing assignments
    /// (M5.2A): transport-attested mTLS principal, a validated collector
    /// PSK, or an explicit isolated-lab opt-in. Version compatibility is
    /// NOT authentication — an unauthenticated dev-compat session gets
    /// rejected assignments instead of device credentials.
    credential_delivery: bool,
    /// Held only for channel liveness: dropping the session (on
    /// unregister/supersede) closes the receiver and ends the stream.
    #[allow(dead_code)]
    tx: mpsc::Sender<AssignmentUpdate>,
}

pub struct CollectorManager {
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    active_collectors: Arc<Mutex<HashMap<String, CollectorSession>>>,
    /// Per-collector monotonically increasing session generation.
    session_generations: Arc<Mutex<HashMap<String, u64>>>,
    /// Per-collector monotonically increasing ASSIGNMENT update
    /// generation (M5.2A). Never resets across sessions — the collector
    /// fences stale/duplicate updates and acknowledges applied
    /// generations via heartbeat so core can confirm withdrawals.
    update_generations: Arc<Mutex<HashMap<String, u64>>>,
    /// (collector_id, device_address) -> generation of the update that
    /// removed the device. A withdrawal is "pending/unconfirmed" until a
    /// heartbeat reports an applied generation >= this value; there is
    /// no claim of remote withdrawal completion without that ack.
    pending_withdrawals: Arc<Mutex<HashMap<(String, String), u64>>>,
    /// collector_id -> per-device acquisition-authority map carried by
    /// the last sent full sync (address -> had live acquisition
    /// authority). Used to diff withdrawals: an address that loses its
    /// assignment OR loses acquisition authority (rejected, disabled,
    /// explicit-empty) is a withdrawal of previous authority. Persists
    /// across sessions so a reconnect full-sync is still tracked.
    last_sent_assignments: Arc<Mutex<HashMap<String, HashMap<String, bool>>>>,
    /// collector_id -> device_address -> authority window
    /// [assigned_gen, removed_gen) within which lifecycle evidence for
    /// that device is valid (M5.2B). `assigned_gen` is the generation of
    /// the sync that first granted acquisition authority and is sticky
    /// across confirming resyncs; `removed_gen` is the first sync that
    /// withdrew it. A lifecycle event is authority-current iff its
    /// stamped generation falls inside the window.
    device_authority_marks: Arc<Mutex<HashMap<String, HashMap<String, DeviceAuthorityMark>>>>,
    runtime_state: Arc<Mutex<HashMap<String, CollectorRuntimeState>>>,
    /// This core boot's assignment-authority epoch (M5.2A). Stamped on
    /// every AssignmentUpdate; the collector fences (epoch, generation)
    /// so a core restart can never be mistaken for continuation of the
    /// previous authority — delayed previous-epoch updates are rejected
    /// and a new epoch is only honoured via a full sync.
    authority_epoch: u64,
    /// Serializes build→stamp→send of full syncs so a slower, older
    /// snapshot can never receive a NEWER generation than a fresher
    /// concurrent resync and supersede more recent desired state.
    send_lock: tokio::sync::Mutex<()>,
    /// Authoritative subscription-session registry (M5.2B): collector
    /// disconnect retires that collector's session authority so delayed
    /// telemetry can never outlive its assignment stream.
    session_registry: Option<Arc<crate::subscription_sessions::SubscriptionSessionRegistry>>,
    /// Routing rules sorted descending by priority (highest first).
    rules: Arc<Mutex<Vec<AssignmentRule>>>,
    /// Cached site records for hierarchy-aware assignment. Updated externally via `set_sites`.
    sites_cache: Arc<Mutex<Vec<SiteRecord>>>,
    /// Optional event sender — wired after construction from GraphStore::event_sender().
    event_tx: Mutex<Option<broadcast::Sender<BonsaiEvent>>>,
    health_emitter: Option<Arc<crate::health_emitter::HealthEmitter>>,
}

#[derive(Clone, Copy, Default)]
struct DeviceAuthorityMark {
    /// Generation of the sync that granted acquisition authority
    /// (sticky across confirming resyncs; 0 = never authorized).
    assigned_gen: u64,
    /// Generation of the first sync that withdrew authority.
    removed_gen: Option<u64>,
}

#[derive(Clone, Default)]
struct CollectorRuntimeState {
    connected: bool,
    queue_depth_updates: u64,
    subscription_count: u32,
    uptime_secs: i64,
    last_heartbeat_ns: i64,
    queue_bytes: u64,
    queue_utilization_pct: f32,
    active_subscribers: u32,
    failed_subscribers: u32,
    memory_used_bytes: u64,
    recent_warn_count: u32,
    recent_error_count: u32,
    receiver_statuses: Vec<ReceiverStatusRecord>,
    /// Last AssignmentUpdate.generation the collector reported applying
    /// (0 = legacy collector without generation reporting).
    applied_assignment_generation: u64,
    /// Authority epoch the collector reported applying (0 = legacy).
    applied_authority_epoch: u64,
}

#[derive(Clone, Default)]
pub struct ReceiverStatusRecord {
    pub name: String,
    pub state: String,
    pub addr: String,
    pub packet_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
}

impl CollectorManager {
    pub fn new(
        registry: Arc<ApiRegistry>,
        credentials: Arc<CredentialVault>,
        initial_rules: Vec<AssignmentRule>,
    ) -> Self {
        let mut rules = initial_rules;
        rules.sort_by_key(|rule| Reverse(rule.priority));
        // Restart/rollback-safe authority epoch (M5.2B): wall-clock
        // nanoseconds alone violate fencing under clock rollback or a
        // restored snapshot. The epoch is max(now, persisted
        // high-water + 1) so every core boot strictly exceeds every
        // previously issued epoch even when the clock moves backwards.
        // Residual: two HA cores booting simultaneously can still mint
        // the same epoch — full fencing needs a durable/consensus
        // authority and is tracked as a separate milestone.
        let persisted_hw = registry
            .sqlite_store()
            .and_then(|store| store.get_setting(ASSIGNMENT_EPOCH_HW_KEY).ok().flatten())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let authority_epoch = (now_ns().max(1) as u64).max(persisted_hw.saturating_add(1));
        if let Some(store) = registry.sqlite_store()
            && let Err(error) =
                store.set_setting(ASSIGNMENT_EPOCH_HW_KEY, &authority_epoch.to_string())
        {
            warn!(%error, "failed to persist assignment authority epoch high-water");
        }
        let manager = Self {
            registry,
            credentials,
            active_collectors: Arc::new(Mutex::new(HashMap::new())),
            session_generations: Arc::new(Mutex::new(HashMap::new())),
            update_generations: Arc::new(Mutex::new(HashMap::new())),
            pending_withdrawals: Arc::new(Mutex::new(HashMap::new())),
            last_sent_assignments: Arc::new(Mutex::new(HashMap::new())),
            device_authority_marks: Arc::new(Mutex::new(HashMap::new())),
            authority_epoch,
            send_lock: tokio::sync::Mutex::new(()),
            session_registry: None,
            runtime_state: Arc::new(Mutex::new(HashMap::new())),
            rules: Arc::new(Mutex::new(rules)),
            sites_cache: Arc::new(Mutex::new(Vec::new())),
            event_tx: Mutex::new(None),
            health_emitter: None,
        };
        manager
    }

    pub fn with_health_emitter(
        mut self,
        emitter: Arc<crate::health_emitter::HealthEmitter>,
    ) -> Self {
        self.health_emitter = Some(emitter);
        self
    }

    /// Bind the authoritative subscription-session registry so collector
    /// unregister retires its session authority (M5.2B).
    pub fn with_session_registry(
        mut self,
        registry: Arc<crate::subscription_sessions::SubscriptionSessionRegistry>,
    ) -> Self {
        self.session_registry = Some(registry);
        self
    }

    #[cfg(test)]
    pub(crate) fn authority_epoch_for_test(&self) -> u64 {
        self.authority_epoch
    }

    /// This core boot's assignment-authority epoch (M5.2B).
    pub fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    /// Latest assignment generation issued to `collector_id` (0 = none).
    pub fn latest_assignment_generation(&self, collector_id: &str) -> u64 {
        self.update_generations
            .lock()
            .unwrap()
            .get(collector_id)
            .copied()
            .unwrap_or(0)
    }

    /// Validate the assignment authority stamped on a subscription
    /// lifecycle event (M5.2B). Returns false when the epoch is not this
    /// core's current authority, the generation was never issued to this
    /// collector, or the generation falls outside the device's
    /// [assigned, removed) authority window — i.e. the event claims
    /// authority the collector did not hold for this device.
    pub fn validate_session_authority(
        &self,
        collector_id: &str,
        device_address: &str,
        authority_epoch: u64,
        assignment_generation: u64,
    ) -> bool {
        if authority_epoch != self.authority_epoch {
            return false;
        }
        if assignment_generation == 0
            || assignment_generation > self.latest_assignment_generation(collector_id)
        {
            return false;
        }
        let marks = self.device_authority_marks.lock().unwrap();
        let Some(device_marks) = marks.get(collector_id) else {
            return false;
        };
        let Some(mark) = device_marks.get(device_address) else {
            return false;
        };
        assignment_generation >= mark.assigned_gen
            && mark
                .removed_gen
                .is_none_or(|removed| assignment_generation < removed)
    }

    /// Wire the graph event channel so collector connect/disconnect events
    /// appear on the SSE stream alongside telemetry events.
    pub fn set_event_sender(&self, tx: broadcast::Sender<BonsaiEvent>) {
        *self.event_tx.lock().expect("event_tx lock poisoned") = Some(tx);
    }

    fn publish_ws(&self, event: BonsaiEvent) {
        if let Some(tx) = self
            .event_tx
            .lock()
            .expect("event_tx lock poisoned")
            .as_ref()
        {
            let _ = tx.send(event);
        }
    }

    /// Returns the current routing rules (sorted by priority descending).
    pub fn get_rules(&self) -> Vec<AssignmentRule> {
        self.rules.lock().expect("rules lock poisoned").clone()
    }

    /// Updates the cached site list used for hierarchy-aware assignment.
    /// Call this on startup and whenever the site graph changes.
    pub fn set_sites(&self, sites: Vec<SiteRecord>) {
        *self.sites_cache.lock().expect("sites lock poisoned") = sites;
    }

    /// Replaces all routing rules and re-evaluates unassigned devices.
    pub fn set_rules(&self, new_rules: Vec<AssignmentRule>) {
        let mut sorted = new_rules;
        sorted.sort_by_key(|rule| Reverse(rule.priority));
        *self.rules.lock().expect("rules lock poisoned") = sorted;
        info!("assignment rules updated; re-evaluating unassigned devices");
        self.reassign_unassigned();
    }

    /// Evaluates routing rules against a target. Returns the matched collector_id or None.
    /// `match_site` on a rule matches the device's own site or any ancestor site (by name or id),
    /// up to a depth of 10. Falls back to exact match when the sites cache is empty.
    pub fn assign_by_rules(&self, target: &TargetConfig) -> Option<String> {
        let rules = self.rules.lock().expect("rules lock poisoned");
        let sites = self.sites_cache.lock().expect("sites lock poisoned");
        let device_site = target.site.as_deref().unwrap_or("");
        let ancestor_set = site_ancestor_set(device_site, &sites);
        let device_role = target.role.as_deref().unwrap_or("");
        for rule in rules.iter() {
            if !ancestor_set.iter().any(|s| s == &rule.match_site) {
                continue;
            }
            if let Some(ref required_role) = rule.match_role
                && required_role != device_role
            {
                continue;
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
            // BN-015-U1: inventory-only assets are declared, not
            // configured — routing rules must never auto-assign them to
            // a collector. Activation requires an explicit collection
            // plan first.
            if target.inventory_only {
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

    /// Register an assignment stream session. Returns the update
    /// receiver plus a session generation that fences `unregister_collector`
    /// against stale-stream cleanup. A newer registration for the same
    /// collector id supersedes the previous session: the old sender is
    /// dropped, ending the old stream, and the old generation can no
    /// longer unregister or trigger device reassignment.
    ///
    /// `credential_delivery` is the authenticated-boundary decision made
    /// by the transport layer (mTLS principal / validated PSK / explicit
    /// lab opt-in). It is NOT derived from protocol version.
    pub async fn register_collector(
        &self,
        collector_id: String,
        protocol_version: u32,
        credential_delivery: bool,
    ) -> Result<(mpsc::Receiver<AssignmentUpdate>, u64)> {
        // G6: Emit metric for collector connection
        metrics::gauge!("bonsai_collector_connected_total").increment(1);

        // G4: Emit health event for collector connection
        if let Some(ref emitter) = self.health_emitter {
            emitter.emit_collector_connected(&collector_id, "unknown");
        }

        let (tx, rx) = mpsc::channel(32);

        // H6: install the session BEFORE building/sending assignments.
        // A concurrent stale-generation cleanup either completes fully
        // before this insert (its generation no longer matches, so it
        // is fenced out) or is still running — in which case this
        // insert waits on the same lock and observes post-cleanup
        // registry state. Assignments are therefore always computed
        // against the state the new session will actually own.
        let generation = {
            let mut generations = self.session_generations.lock().unwrap();
            let next = generations.entry(collector_id.clone()).or_insert(0);
            *next += 1;
            *next
        };
        {
            let mut collectors = self.active_collectors.lock().unwrap();
            collectors.insert(
                collector_id.clone(),
                CollectorSession {
                    generation,
                    protocol_version,
                    credential_delivery,
                    tx: tx.clone(),
                },
            );
        }
        {
            let mut runtime = self.runtime_state.lock().unwrap();
            let entry = runtime.entry(collector_id.clone()).or_default();
            entry.connected = true;
            entry.last_heartbeat_ns = now_ns();
        }

        if !self
            .deliver_full_sync(&collector_id, &tx, protocol_version, credential_delivery)
            .await
        {
            // Receiver already gone — do not leak the session.
            self.unregister_collector(&collector_id, generation);
            anyhow::bail!("assignment stream receiver closed during registration");
        }

        self.publish_ws(BonsaiEvent {
            device_address: collector_id.clone(),
            event_type: "collector_status_change".to_string(),
            detail_json: format!(r#"{{"collector_id":"{}","connected":true}}"#, collector_id),
            occurred_at_ns: now_ns(),
            state_change_event_id: String::new(),
            source_type: "collector".to_string(),
        });
        info!(%collector_id, generation, "collector registered, initial assignments sent");
        Ok((rx, generation))
    }

    /// Unregister a session. Only the CURRENT generation may remove the
    /// session and run device-reassignment cleanup — a stale stream
    /// closing after a reconnect is a no-op and must not disconnect the
    /// newer valid session or orphan its devices.
    pub fn unregister_collector(&self, collector_id: &str, generation: u64) -> bool {
        // H6: the generation check AND all cleanup run under one
        // `active_collectors` lock. `register_collector` inserts new
        // sessions under the same lock, so a stale cleanup either sees
        // the newer generation and returns immediately, or completes
        // entirely before the new session exists — it can never mark a
        // newer session disconnected, clear its assignments, or deliver
        // stale assignment state.
        let mut collectors = self.active_collectors.lock().unwrap();
        let removed = match collectors.get(collector_id) {
            Some(session) if session.generation == generation => {
                collectors.remove(collector_id).is_some()
            }
            _ => false,
        };
        if !removed {
            info!(
                %collector_id,
                generation,
                "stale collector session close ignored (superseded or already unregistered)"
            );
            return false;
        }
        // G6: Emit metric for collector disconnection
        metrics::gauge!("bonsai_collector_connected_total").decrement(1);

        // G4: Emit health event for collector disconnection
        if let Some(ref emitter) = self.health_emitter {
            emitter.emit_collector_disconnected(collector_id, "unknown");
        }

        // M5.2B: collector disconnect retires its subscription-session
        // authority immediately — delayed telemetry from those sessions
        // stays historical and can never satisfy current coverage.
        if let Some(registry) = &self.session_registry {
            registry.retire_collector(collector_id);
        }

        {
            let mut runtime = self.runtime_state.lock().unwrap();
            if let Some(entry) = runtime.get_mut(collector_id) {
                entry.connected = false;
            }
        }
        self.publish_ws(BonsaiEvent {
            device_address: collector_id.to_string(),
            event_type: "collector_status_change".to_string(),
            detail_json: format!(r#"{{"collector_id":"{}","connected":false}}"#, collector_id),
            occurred_at_ns: now_ns(),
            state_change_event_id: String::new(),
            source_type: "collector".to_string(),
        });
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
        true
    }

    pub fn record_heartbeat(
        &self,
        collector_id: &str,
        queue_depth_updates: u64,
        subscription_count: u32,
        uptime_secs: i64,
        queue_bytes: u64,
        queue_utilization_pct: f32,
        active_subscribers: u32,
        failed_subscribers: u32,
        memory_used_bytes: u64,
        recent_warn_count: u32,
        recent_error_count: u32,
        receiver_statuses: Vec<ReceiverStatusRecord>,
        applied_assignment_generation: u64,
        authority_epoch: u64,
    ) {
        {
            let mut runtime = self.runtime_state.lock().unwrap();
            let entry = runtime.entry(collector_id.to_string()).or_default();
            entry.connected = true;
            entry.queue_depth_updates = queue_depth_updates;
            entry.subscription_count = subscription_count;
            entry.uptime_secs = uptime_secs;
            entry.last_heartbeat_ns = now_ns();
            entry.queue_bytes = queue_bytes;
            entry.queue_utilization_pct = queue_utilization_pct;
            entry.active_subscribers = active_subscribers;
            entry.failed_subscribers = failed_subscribers;
            entry.memory_used_bytes = memory_used_bytes;
            entry.recent_warn_count = recent_warn_count;
            entry.recent_error_count = recent_error_count;
            entry.receiver_statuses = receiver_statuses;
            entry.applied_assignment_generation = applied_assignment_generation;
            entry.applied_authority_epoch = authority_epoch;
        }
        // Withdrawal acknowledgement (M5.2A): a heartbeat confirms a
        // withdrawal ONLY when it is bound to the current authority
        // epoch AND reports an applied generation that was actually
        // issued to this collector (required <= applied <= last issued).
        // An arbitrary future generation, a stale-epoch ack, or a legacy
        // collector (epoch 0) cannot clear pending withdrawals.
        if authority_epoch == self.authority_epoch && applied_assignment_generation > 0 {
            let issued = self
                .update_generations
                .lock()
                .unwrap()
                .get(collector_id)
                .copied()
                .unwrap_or(0);
            let mut pending = self.pending_withdrawals.lock().unwrap();
            let before = pending.len();
            pending.retain(|(cid, _), required| {
                !(cid == collector_id
                    && applied_assignment_generation >= *required
                    && applied_assignment_generation <= issued)
            });
            let confirmed = before - pending.len();
            if confirmed > 0 {
                info!(
                    %collector_id,
                    confirmed,
                    applied_assignment_generation,
                    "collector acknowledged assignment withdrawals"
                );
            }
        }
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
                Some(col_id) => by_collector
                    .entry(col_id.clone())
                    .or_default()
                    .push(target.address.clone()),
                None => unassigned_devices.push(target.address.clone()),
            }
        }

        // Connected collectors first, then known-but-disconnected ones.
        let mut all_ids: std::collections::HashSet<String> =
            connected_ids.iter().cloned().collect();
        all_ids.extend(by_collector.keys().cloned());
        all_ids.extend(runtime.keys().cloned());

        let pending = self.pending_withdrawals.lock().unwrap();
        let mut collectors: Vec<CollectorStatus> = all_ids
            .into_iter()
            .map(|id| {
                let assigned = by_collector.get(&id).cloned().unwrap_or_default();
                let runtime_state = runtime.get(&id).cloned().unwrap_or_default();
                let connected = connected_ids.contains(&id) || runtime_state.connected;
                let pending_withdrawal_devices = pending
                    .iter()
                    .filter(|((cid, _), _)| cid == &id)
                    .map(|((_, addr), _)| addr.clone())
                    .collect();
                CollectorStatus {
                    id,
                    connected,
                    assigned_device_count: assigned.len(),
                    assigned_targets: assigned,
                    queue_depth_updates: runtime_state.queue_depth_updates,
                    subscription_count: runtime_state.subscription_count,
                    uptime_secs: runtime_state.uptime_secs,
                    last_heartbeat_ns: runtime_state.last_heartbeat_ns,
                    queue_bytes: runtime_state.queue_bytes,
                    queue_utilization_pct: runtime_state.queue_utilization_pct,
                    active_subscribers: runtime_state.active_subscribers,
                    failed_subscribers: runtime_state.failed_subscribers,
                    memory_used_bytes: runtime_state.memory_used_bytes,
                    recent_warn_count: runtime_state.recent_warn_count,
                    recent_error_count: runtime_state.recent_error_count,
                    receiver_statuses: runtime_state.receiver_statuses,
                    pending_withdrawal_devices,
                }
            })
            .collect();
        collectors.sort_by(|a, b| a.id.cmp(&b.id));

        CollectorStatusSummary {
            collectors,
            unassigned_devices,
        }
    }

    /// Build the DeviceAssignment list for a collector session, enforcing
    /// the acquisition and transport-security contracts: devices with
    /// non-Automatic acquisition intent are only delivered to collectors
    /// at or above ACQUISITION_CONTRACT_VERSION, and mTLS devices only to
    /// collectors at or above SECURITY_CONTRACT_VERSION (older collectors
    /// would silently drop the client identity and downgrade to plain TLS
    /// verify). Credential-bearing assignments additionally require an
    /// authenticated session boundary (`credential_delivery`) — version
    /// compatibility is not authentication. Returns (assignments,
    /// blocked (address, audit reason)).
    fn build_assignments(
        &self,
        collector_id: &str,
        protocol_version: u32,
        credential_delivery: bool,
    ) -> (Vec<DeviceAssignment>, Vec<(String, &'static str)>) {
        let targets = match self.registry.list_assigned_to(collector_id) {
            Ok(targets) => targets,
            Err(e) => {
                warn!(%collector_id, %e, "failed to list assigned targets");
                return (Vec::new(), Vec::new());
            }
        };
        let acquisition_aware = protocol_version >= ACQUISITION_CONTRACT_VERSION;
        let security_aware = protocol_version >= SECURITY_CONTRACT_VERSION;
        let lifecycle_aware = protocol_version >= ASSIGNMENT_LIFECYCLE_CONTRACT_VERSION;
        let mut assignments = Vec::new();
        let mut blocked = Vec::new();
        for target in targets {
            if !acquisition_aware && target.acquisition != AcquisitionMode::Automatic {
                blocked.push((
                    target.address.clone(),
                    "acquisition_requires_newer_collector",
                ));
                continue;
            }
            if !security_aware && target.security == Some(crate::config::GnmiSecurityMode::Mtls) {
                blocked.push((target.address.clone(), "security_requires_newer_collector"));
                continue;
            }
            let mut assignment =
                match try_create_assignment(&target, &self.credentials, &self.registry) {
                    Ok(assignment) => assignment,
                    Err(reject_reason) => rejected_assignment(&target, reject_reason),
                };
            // Credential-bearing assignments require the authenticated
            // boundary decided at registration — never implied by version.
            if !assignment.rejected
                && !credential_delivery
                && assignment_carries_credentials(&assignment)
            {
                assignment.username.clear();
                assignment.password.clear();
                assignment.rejected = true;
                assignment.reject_reason =
                    "credentials_require_authenticated_collector".to_string();
            }
            // A collector that predates the lifecycle contract cannot
            // express rejected/unavailable outcomes — delivering one
            // would be silently misread as a usable assignment. Withhold
            // and unassign instead (fail closed).
            if assignment.rejected && !lifecycle_aware {
                blocked.push((
                    target.address.clone(),
                    "assignment_requires_newer_collector",
                ));
                continue;
            }
            assignments.push(assignment);
        }
        (assignments, blocked)
    }

    /// Build, gate, version-stamp and deliver a full assignment sync.
    /// The whole build→stamp→send sequence is serialized per manager so
    /// a slower older snapshot can never receive a NEWER generation than
    /// a fresher concurrent resync. Stamps the current authority_epoch.
    /// Records per-device authority withdrawals against the update
    /// generation so core can distinguish "sent" from "acknowledged".
    /// Returns false when the session channel is already closed.
    async fn deliver_full_sync(
        &self,
        collector_id: &str,
        tx: &mpsc::Sender<AssignmentUpdate>,
        protocol_version: u32,
        credential_delivery: bool,
    ) -> bool {
        // Serialize snapshot+stamp+send: generation order must match
        // desired-state order or an older config could supersede newer.
        let _send_guard = self.send_lock.lock().await;
        let (assignments, blocked) =
            self.build_assignments(collector_id, protocol_version, credential_delivery);
        for (address, reason) in &blocked {
            // Fail closed AND visible: an incompatible collector must never
            // receive intent it cannot enforce (it would interpret explicit
            // acquisition as automatic, silently drop mTLS client identity,
            // or misread a rejected assignment as usable), so the device is
            // unassigned and the operator sees it return to the unassigned
            // pool.
            warn!(
                %collector_id,
                device = %address,
                protocol_version,
                reason,
                "assignment withheld: collector predates a required contract"
            );
            metrics::counter!("bonsai_assignment_blocked_protocol_total").increment(1);
            if let Err(e) = self
                .registry
                .assign_device_with_audit(address, None, "system", reason)
            {
                warn!(device = %address, %e, "failed to unassign incompatible device");
            }
        }

        let generation = {
            let mut generations = self.update_generations.lock().unwrap();
            let next = generations.entry(collector_id.to_string()).or_insert(0);
            *next += 1;
            *next
        };

        // Withdrawal diff on ACQUISITION AUTHORITY, not mere presence:
        // an address that disappears OR loses live authority (rejected,
        // disabled, explicit-empty) withdraws the previous authority.
        // Pending until an epoch-bound heartbeat acknowledges this
        // generation.
        let current: HashMap<String, bool> = assignments
            .iter()
            .filter_map(|a| {
                a.device
                    .as_ref()
                    .map(|d| (d.address.clone(), assignment_has_acquisition_authority(a)))
            })
            .collect();
        {
            let mut last_sent = self.last_sent_assignments.lock().unwrap();
            if let Some(previous) = last_sent.insert(collector_id.to_string(), current.clone()) {
                let mut pending = self.pending_withdrawals.lock().unwrap();
                for (address, had_authority) in previous {
                    let still_authoritative = current.get(&address).copied().unwrap_or(false);
                    if had_authority && !still_authoritative {
                        pending.insert((collector_id.to_string(), address), generation);
                    }
                }
            }
        }

        // Authority windows for lifecycle validation (M5.2B): the first
        // sync granting authority fixes `assigned_gen`; the first sync
        // withdrawing it fixes `removed_gen`. Confirming resyncs leave
        // an authorized mark untouched so sessions spawned under an
        // earlier generation remain inside their authority window.
        {
            let mut marks = self.device_authority_marks.lock().unwrap();
            let device_marks = marks.entry(collector_id.to_string()).or_default();
            let addresses: std::collections::HashSet<String> = device_marks
                .keys()
                .cloned()
                .chain(current.keys().cloned())
                .collect();
            for address in addresses {
                if current.get(&address).copied().unwrap_or(false) {
                    let still_authorized = device_marks
                        .get(&address)
                        .is_some_and(|m| m.assigned_gen != 0 && m.removed_gen.is_none());
                    if !still_authorized {
                        device_marks.insert(
                            address,
                            DeviceAuthorityMark {
                                assigned_gen: generation,
                                removed_gen: None,
                            },
                        );
                    }
                } else {
                    let mark = device_marks
                        .entry(address)
                        .or_insert(DeviceAuthorityMark::default());
                    if mark.removed_gen.is_none() {
                        mark.removed_gen = Some(generation);
                    }
                }
            }
        }

        tx.send(AssignmentUpdate {
            assignments,
            is_full_sync: true,
            generation,
            authority_epoch: self.authority_epoch,
        })
        .await
        .is_ok()
    }

    /// Re-send a full assignment sync to a live collector session. Used when
    /// a device's acquisition intent changes so an obsolete broader
    /// subscription is withdrawn promptly instead of surviving until the
    /// next reconnect.
    pub async fn resync_collector(&self, collector_id: &str) {
        let (tx, version, credential_delivery) = {
            let collectors = self.active_collectors.lock().unwrap();
            match collectors.get(collector_id) {
                Some(session) => (
                    session.tx.clone(),
                    session.protocol_version,
                    session.credential_delivery,
                ),
                None => return,
            }
        };
        if !self
            .deliver_full_sync(collector_id, &tx, version, credential_delivery)
            .await
        {
            warn!(%collector_id, "assignment resync dropped: session channel closed");
        }
    }

    /// Whether the named collector session can enforce this target's
    /// acquisition intent. Unknown/offline collectors return true — the
    /// delivery-time filter in `build_assignments` is the enforcement point.
    pub fn collector_supports_acquisition(
        &self,
        collector_id: &str,
        target: &TargetConfig,
    ) -> bool {
        if target.acquisition == AcquisitionMode::Automatic {
            return true;
        }
        let collectors = self.active_collectors.lock().unwrap();
        match collectors.get(collector_id) {
            Some(session) => session.protocol_version >= ACQUISITION_CONTRACT_VERSION,
            None => true,
        }
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
    pub queue_bytes: u64,
    pub queue_utilization_pct: f32,
    pub active_subscribers: u32,
    pub failed_subscribers: u32,
    pub memory_used_bytes: u64,
    pub recent_warn_count: u32,
    pub recent_error_count: u32,
    pub receiver_statuses: Vec<ReceiverStatusRecord>,
    /// Devices removed from this collector's assignment set whose
    /// withdrawal has NOT yet been acknowledged by an applied-generation
    /// heartbeat. Non-empty means remote withdrawal is pending — never
    /// report it as complete.
    pub pending_withdrawal_devices: Vec<String>,
}

pub struct CollectorStatusSummary {
    pub collectors: Vec<CollectorStatus>,
    pub unassigned_devices: Vec<String>,
}

/// Returns the set of site identifiers (name and id) reachable by walking `device_site`
/// up through the parent chain, capped at depth 10. The device's own site string is
/// always included as the first element (exact-match fallback when the cache is empty).
fn site_ancestor_set(device_site: &str, sites: &[SiteRecord]) -> Vec<String> {
    let mut result = vec![device_site.to_string()];
    if device_site.is_empty() || sites.is_empty() {
        return result;
    }
    let mut current_key = device_site.to_string();
    for _ in 0..10 {
        let Some(rec) = sites
            .iter()
            .find(|s| s.name == current_key || s.id == current_key)
        else {
            break;
        };
        for val in [&rec.name, &rec.id] {
            if !val.is_empty() && !result.contains(val) {
                result.push(val.clone());
            }
        }
        if rec.parent_id.is_empty() || result.contains(&rec.parent_id) {
            break;
        }
        current_key = rec.parent_id.clone();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_site(id: &str, name: &str, parent_id: &str) -> SiteRecord {
        SiteRecord {
            id: id.to_string(),
            name: name.to_string(),
            parent_id: parent_id.to_string(),
            kind: "dc".to_string(),
            lat: 0.0,
            lon: 0.0,
            metadata_json: "{}".to_string(),
            environment_id: String::new(),
        }
    }

    #[test]
    fn exact_match_without_hierarchy() {
        let sites = vec![make_site("s1", "dc-london", "")];
        let result = site_ancestor_set("dc-london", &sites);
        assert!(result.contains(&"dc-london".to_string()));
        assert!(result.contains(&"s1".to_string()));
    }

    #[test]
    fn child_site_matches_parent_rule() {
        let sites = vec![
            make_site("parent", "dc-london", ""),
            make_site("child", "rack-london-a1", "parent"),
        ];
        let result = site_ancestor_set("rack-london-a1", &sites);
        assert!(result.contains(&"rack-london-a1".to_string()));
        assert!(result.contains(&"dc-london".to_string()));
        assert!(result.contains(&"parent".to_string()));
    }

    #[test]
    fn three_level_hierarchy() {
        let sites = vec![
            make_site("root", "region-eu", ""),
            make_site("mid", "dc-london", "root"),
            make_site("leaf", "rack-london-a1", "mid"),
        ];
        let result = site_ancestor_set("rack-london-a1", &sites);
        assert!(result.contains(&"rack-london-a1".to_string()));
        assert!(result.contains(&"dc-london".to_string()));
        assert!(result.contains(&"region-eu".to_string()));
    }

    #[test]
    fn cycle_does_not_loop_forever() {
        let sites = vec![make_site("a", "site-a", "b"), make_site("b", "site-b", "a")];
        let result = site_ancestor_set("site-a", &sites);
        assert!(result.len() <= 12); // bounded by depth cap + seed
    }

    #[test]
    fn empty_cache_falls_back_to_exact_match() {
        let result = site_ancestor_set("dc-london", &[]);
        assert_eq!(result, vec!["dc-london".to_string()]);
    }

    fn acquisition_target(address: &str, acquisition: AcquisitionMode) -> TargetConfig {
        TargetConfig {
            acquisition,
            extra_ips: Vec::new(),
            optional: false,
            paths: Vec::new(),
            address: address.to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: None,
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: None,
            role: None,
            site: None,
            collector_id: Some("c1".to_string()),
            selected_paths: Vec::new(),
            external_id: None,
            inventory_only: false,
            inventory_platform: None,
            inventory_status: None,
            inventory_tenant: None,
            security: None,
            client_cert: None,
            client_key: None,
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
        }
    }

    async fn manager_with_device(
        acquisition: AcquisitionMode,
    ) -> (CollectorManager, Arc<ApiRegistry>, tempfile::TempDir) {
        unsafe { std::env::set_var("BONSAI_ASSIGNMENT_TEST_VAULT", "test-passphrase") };
        let dir = tempfile::tempdir().unwrap();
        let registry =
            Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
        let credentials = Arc::new(
            CredentialVault::open(dir.path().join("vault"), "BONSAI_ASSIGNMENT_TEST_VAULT")
                .unwrap(),
        );
        let target = acquisition_target("10.0.0.1", acquisition);
        registry
            .add_device_with_audit(target.clone(), "test", "test_add")
            .unwrap();
        registry
            .assign_device_with_audit(
                &target.address,
                Some("c1".to_string()),
                "test",
                "test_assign",
            )
            .unwrap();
        (
            CollectorManager::new(registry.clone(), credentials, vec![]),
            registry,
            dir,
        )
    }

    #[tokio::test]
    async fn pre_contract_collector_never_receives_explicit_assignment() {
        let (manager, registry, _dir) = manager_with_device(AcquisitionMode::Explicit).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), ACQUISITION_CONTRACT_VERSION - 1, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert!(update.is_full_sync);
        assert!(
            update.assignments.is_empty(),
            "explicit plan must not reach a pre-contract collector"
        );
        // The device is unassigned — operator-visible, not silently delivered.
        assert!(
            registry
                .get_device("10.0.0.1")
                .unwrap()
                .unwrap()
                .collector_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn pre_contract_collector_never_receives_disabled_assignment() {
        let (manager, registry, _dir) = manager_with_device(AcquisitionMode::Disabled).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), 1, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert!(update.assignments.is_empty());
        assert!(
            registry
                .get_device("10.0.0.1")
                .unwrap()
                .unwrap()
                .collector_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn pre_contract_collector_still_receives_automatic_assignment() {
        let (manager, _registry, _dir) = manager_with_device(AcquisitionMode::Automatic).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), 1, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert_eq!(update.assignments.len(), 1);
    }

    #[tokio::test]
    async fn contract_collector_receives_explicit_assignment() {
        let (manager, _registry, _dir) = manager_with_device(AcquisitionMode::Explicit).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), ACQUISITION_CONTRACT_VERSION, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert_eq!(update.assignments.len(), 1);
        let device = update.assignments[0].device.as_ref().unwrap();
        assert_eq!(device.acquisition, "explicit");
    }

    async fn manager_with_mtls_device() -> (CollectorManager, Arc<ApiRegistry>, tempfile::TempDir) {
        unsafe { std::env::set_var("BONSAI_ASSIGNMENT_TEST_VAULT", "test-passphrase") };
        let dir = tempfile::tempdir().unwrap();
        let registry =
            Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
        let credentials = Arc::new(
            CredentialVault::open(dir.path().join("vault"), "BONSAI_ASSIGNMENT_TEST_VAULT")
                .unwrap(),
        );
        let mut target = acquisition_target("10.0.0.2", AcquisitionMode::Automatic);
        target.security = Some(crate::config::GnmiSecurityMode::Mtls);
        target.ca_cert = Some("ca.pem".to_string());
        target.tls_domain = Some("device.example".to_string());
        target.client_cert = Some("client.pem".to_string());
        target.client_key = Some("client.key".to_string());
        registry
            .add_device_with_audit(target.clone(), "test", "test_add")
            .unwrap();
        registry
            .assign_device_with_audit(
                &target.address,
                Some("c1".to_string()),
                "test",
                "test_assign",
            )
            .unwrap();
        (
            CollectorManager::new(registry.clone(), credentials, vec![]),
            registry,
            dir,
        )
    }

    #[tokio::test]
    async fn pre_security_collector_never_receives_mtls_assignment() {
        let (manager, registry, _dir) = manager_with_mtls_device().await;
        // v2 understands the acquisition contract but predates security
        // intent — delivering mtls would silently drop client identity.
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), SECURITY_CONTRACT_VERSION - 1, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert!(
            update.assignments.is_empty(),
            "mTLS must not reach a pre-security collector"
        );
        assert!(
            registry
                .get_device("10.0.0.2")
                .unwrap()
                .unwrap()
                .collector_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn security_collector_receives_mtls_assignment() {
        let (manager, _registry, _dir) = manager_with_mtls_device().await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), SECURITY_CONTRACT_VERSION, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert_eq!(update.assignments.len(), 1);
        let device = update.assignments[0].device.as_ref().unwrap();
        assert_eq!(device.security, "mtls");
        assert_eq!(device.client_cert, "client.pem");
        assert_eq!(device.client_key, "client.key");
    }

    async fn manager_with_custom_target(
        target: TargetConfig,
    ) -> (CollectorManager, Arc<ApiRegistry>, tempfile::TempDir) {
        unsafe { std::env::set_var("BONSAI_ASSIGNMENT_TEST_VAULT", "test-passphrase") };
        let dir = tempfile::tempdir().unwrap();
        let registry =
            Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
        let credentials = Arc::new(
            CredentialVault::open(dir.path().join("vault"), "BONSAI_ASSIGNMENT_TEST_VAULT")
                .unwrap(),
        );
        registry
            .add_device_with_audit(target.clone(), "test", "test_add")
            .unwrap();
        registry
            .assign_device_with_audit(
                &target.address,
                Some("c1".to_string()),
                "test",
                "test_assign",
            )
            .unwrap();
        (
            CollectorManager::new(registry.clone(), credentials, vec![]),
            registry,
            dir,
        )
    }

    #[tokio::test]
    async fn unresolvable_credential_alias_is_rejected_never_anonymous() {
        let mut target = acquisition_target("10.0.0.3", AcquisitionMode::Automatic);
        target.credential_alias = Some("no-such-alias".to_string());
        let (manager, _registry, _dir) = manager_with_custom_target(target).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert_eq!(update.assignments.len(), 1);
        let assignment = &update.assignments[0];
        assert!(assignment.rejected, "unresolvable alias must reject");
        assert!(
            assignment.reject_reason.contains("credential_unavailable"),
            "reason: {}",
            assignment.reject_reason
        );
        assert!(assignment.username.is_empty() && assignment.password.is_empty());
    }

    #[tokio::test]
    async fn credential_bearing_assignment_requires_authenticated_boundary() {
        let mut target = acquisition_target("10.0.0.4", AcquisitionMode::Automatic);
        target.username = Some("lab".to_string());
        target.password = Some("lab-pass".to_string());
        let (manager, _registry, _dir) = manager_with_custom_target(target.clone()).await;

        // Unauthenticated session (no mTLS principal, no PSK, no lab
        // opt-in): credentials must be stripped and the assignment
        // rejected — version compatibility is not authentication.
        let (mut rx, _gen) = manager
            .register_collector(
                "c1".to_string(),
                crate::api::PROTOCOL_VERSION_CURRENT,
                false,
            )
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        let assignment = &update.assignments[0];
        assert!(assignment.rejected);
        assert_eq!(
            assignment.reject_reason,
            "credentials_require_authenticated_collector"
        );
        assert!(assignment.username.is_empty() && assignment.password.is_empty());
        drop(rx);

        // Authenticated boundary: credentials delivered.
        let (mut rx2, _gen2) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let update2 = rx2.recv().await.expect("resync");
        let assignment2 = &update2.assignments[0];
        assert!(!assignment2.rejected);
        assert_eq!(assignment2.username, "lab");
        assert_eq!(assignment2.password, "lab-pass");
    }

    #[tokio::test]
    async fn lifecycle_unaware_collector_never_receives_rejected_assignment() {
        let mut target = acquisition_target("10.0.0.5", AcquisitionMode::Automatic);
        target.credential_alias = Some("no-such-alias".to_string());
        let (manager, registry, _dir) = manager_with_custom_target(target).await;
        // v3 understands credentials but not rejected/unavailable — the
        // assignment must be withheld entirely (fail closed), not
        // delivered as apparently usable.
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), SECURITY_CONTRACT_VERSION, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        assert!(
            update.assignments.is_empty(),
            "rejected assignment must not reach a lifecycle-unaware collector"
        );
        assert!(
            registry
                .get_device("10.0.0.5")
                .unwrap()
                .unwrap()
                .collector_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn partial_inline_credentials_are_rejected() {
        let mut target = acquisition_target("10.0.0.6", AcquisitionMode::Automatic);
        target.username = Some("lab".to_string());
        // password missing → incomplete credential metadata, explicit
        // rejection rather than silent empty-password completion.
        let (manager, _registry, _dir) = manager_with_custom_target(target).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let update = rx.recv().await.expect("initial sync");
        let assignment = &update.assignments[0];
        assert!(assignment.rejected);
        assert!(assignment.reject_reason.contains("credential_incomplete"));
    }

    #[tokio::test]
    async fn assignment_update_generation_is_stamped_and_monotonic() {
        let (manager, _registry, _dir) = manager_with_device(AcquisitionMode::Automatic).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let first = rx.recv().await.expect("initial sync");
        assert!(first.generation > 0);
        // Trigger a resync — generation must strictly increase.
        manager.resync_collector("c1").await;
        let second = rx.recv().await.expect("resync");
        assert!(second.generation > first.generation);
        assert_eq!(second.authority_epoch, first.authority_epoch);
        assert!(second.authority_epoch > 0);
    }

    #[tokio::test]
    async fn authority_epoch_differs_across_core_instances() {
        // Simulates a core restart: a NEW CollectorManager over the same
        // kind of state must stamp a different (newer) epoch so delayed
        // previous-authority updates stay fenced collector-side.
        let (_m1, _r1, _d1) = manager_with_device(AcquisitionMode::Automatic).await;
        let epoch1 = _m1.authority_epoch_for_test();
        let (_m2, _r2, _d2) = manager_with_device(AcquisitionMode::Automatic).await;
        let epoch2 = _m2.authority_epoch_for_test();
        assert_ne!(epoch1, epoch2, "core restart must mint a new epoch");
        assert!(epoch2 > epoch1);
    }

    fn heartbeat_args(
        collector_id: &str,
        applied_generation: u64,
        authority_epoch: u64,
    ) -> (
        &str,
        u64,
        u32,
        i64,
        u64,
        f32,
        u32,
        u32,
        u64,
        u32,
        u32,
        Vec<ReceiverStatusRecord>,
        u64,
        u64,
    ) {
        (
            collector_id,
            0,
            0,
            0,
            0,
            0.0,
            0,
            0,
            0,
            0,
            0,
            Vec::new(),
            applied_generation,
            authority_epoch,
        )
    }

    #[tokio::test]
    async fn withdrawal_ack_requires_current_epoch_and_issued_generation() {
        let (manager, registry, _dir) = manager_with_device(AcquisitionMode::Automatic).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let first = rx.recv().await.expect("initial sync");
        let epoch = first.authority_epoch;
        let issued_gen = first.generation;

        // Withdraw the device, then resync → withdrawal pending at gen2.
        registry
            .assign_device_with_audit("10.0.0.1", None, "test", "unassign")
            .unwrap();
        manager.resync_collector("c1").await;
        let second = rx.recv().await.expect("resync");
        assert!(second.generation > issued_gen);
        let pending_of = |m: &CollectorManager| {
            m.collector_status_summary()
                .collectors
                .iter()
                .find(|c| c.id == "c1")
                .map(|c| c.pending_withdrawal_devices.clone())
                .unwrap_or_default()
        };
        assert_eq!(pending_of(&manager), vec!["10.0.0.1".to_string()]);

        // Arbitrary FUTURE generation cannot clear the withdrawal.
        let args = heartbeat_args("c1", issued_gen + 100, epoch);
        manager.record_heartbeat(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
            args.10, args.11, args.12, args.13,
        );
        assert_eq!(pending_of(&manager), vec!["10.0.0.1".to_string()]);

        // Stale-epoch ack cannot clear it either.
        let args = heartbeat_args("c1", second.generation, epoch + 1);
        manager.record_heartbeat(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
            args.10, args.11, args.12, args.13,
        );
        assert_eq!(pending_of(&manager), vec!["10.0.0.1".to_string()]);

        // Correct epoch + issued generation → confirmed.
        let args = heartbeat_args("c1", second.generation, epoch);
        manager.record_heartbeat(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
            args.10, args.11, args.12, args.13,
        );
        assert!(pending_of(&manager).is_empty());
    }

    #[tokio::test]
    async fn authority_loss_while_still_assigned_is_a_withdrawal() {
        // Explicit acquisition WITH a selected path = live authority.
        let mut explicit_target = acquisition_target("10.0.0.1", AcquisitionMode::Explicit);
        explicit_target.selected_paths = vec![crate::config::SelectedSubscriptionPath {
            path: "/system".to_string(),
            origin: "openconfig".to_string(),
            provenance: String::new(),
            mode: "sample".to_string(),
            sample_interval_ns: 1_000_000_000,
            rationale: String::new(),
            optional: false,
        }];
        let (manager, registry, _dir) = manager_with_custom_target(explicit_target).await;
        let (mut rx, _gen) = manager
            .register_collector("c1".to_string(), crate::api::PROTOCOL_VERSION_CURRENT, true)
            .await
            .unwrap();
        let _ = rx.recv().await.expect("initial sync");

        // Same device stays assigned but loses acquisition authority:
        // explicit → disabled. The address remains in the assignment set
        // yet the previous authority is withdrawn.
        let mut target = acquisition_target("10.0.0.1", AcquisitionMode::Disabled);
        target.selected_paths = vec![crate::config::SelectedSubscriptionPath {
            path: "/system".to_string(),
            origin: "openconfig".to_string(),
            provenance: String::new(),
            mode: "sample".to_string(),
            sample_interval_ns: 1_000_000_000,
            rationale: String::new(),
            optional: false,
        }];
        registry
            .update_device_with_audit(target, "test", "disable")
            .unwrap();
        // Ensure the device remains assigned — the withdrawal must be
        // due to lost AUTHORITY, not lost assignment.
        registry
            .assign_device_with_audit("10.0.0.1", Some("c1".to_string()), "test", "keep")
            .unwrap();
        manager.resync_collector("c1").await;
        let _ = rx.recv().await.expect("resync");

        let pending = manager
            .collector_status_summary()
            .collectors
            .iter()
            .find(|c| c.id == "c1")
            .map(|c| c.pending_withdrawal_devices.clone())
            .unwrap_or_default();
        assert_eq!(pending, vec!["10.0.0.1".to_string()]);
    }

    /// M5.2B: lifecycle authority windows — a lifecycle event is valid
    /// only inside [assigned_gen, removed_gen) for its (collector,
    /// device) pair within the current authority epoch.
    #[tokio::test]
    async fn session_authority_window_accepts_only_current_assignment() {
        let (manager, _registry, _dir) =
            manager_with_device(AcquisitionMode::Explicit).await;
        let epoch = manager.authority_epoch();
        let (mut rx, _gen) = manager
            .register_collector(
                "c1".to_string(),
                crate::api::PROTOCOL_VERSION_CURRENT,
                true,
            )
            .await
            .unwrap();
        let assigned_gen = rx.recv().await.expect("initial sync").generation;
        assert!(assigned_gen > 0);

        // The assigning generation itself is inside the window.
        assert!(manager.validate_session_authority(
            "c1",
            "10.0.0.1",
            epoch,
            assigned_gen
        ));
        // Never-issued future generations and the zero generation are
        // not authority this collector holds.
        assert!(!manager.validate_session_authority(
            "c1",
            "10.0.0.1",
            epoch,
            assigned_gen + 1
        ));
        assert!(!manager.validate_session_authority(
            "c1", "10.0.0.1", epoch, 0
        ));
        // A delayed event from a different authority epoch is fenced.
        assert!(!manager.validate_session_authority(
            "c1",
            "10.0.0.1",
            epoch + 1,
            assigned_gen
        ));
        // Another collector or an unknown device holds no window.
        assert!(!manager.validate_session_authority(
            "c2",
            "10.0.0.1",
            epoch,
            assigned_gen
        ));
        assert!(!manager.validate_session_authority(
            "c1",
            "10.0.0.2",
            epoch,
            assigned_gen
        ));
    }

    /// Removal closes the window at the withdrawing generation;
    /// reassignment opens a fresh window and fences every stale
    /// generation from prior windows.
    #[tokio::test]
    async fn session_authority_window_closes_on_removal_and_reassignment() {
        let (manager, registry, _dir) =
            manager_with_device(AcquisitionMode::Explicit).await;
        let epoch = manager.authority_epoch();
        let (mut rx, _gen) = manager
            .register_collector(
                "c1".to_string(),
                crate::api::PROTOCOL_VERSION_CURRENT,
                true,
            )
            .await
            .unwrap();
        let g1 = rx.recv().await.expect("initial sync").generation;

        // Withdraw the device: the resync generation becomes the
        // exclusive upper bound of the authority window.
        registry
            .assign_device_with_audit("10.0.0.1", None, "test", "unassign")
            .unwrap();
        manager.resync_collector("c1").await;
        let g2 = rx.recv().await.expect("withdrawal sync").generation;
        assert!(g2 > g1);

        // Generations strictly inside [g1, g2) remain valid evidence of
        // the authority held at issuance; the boundary and later are not.
        assert!(manager.validate_session_authority("c1", "10.0.0.1", epoch, g1));
        assert!(!manager.validate_session_authority("c1", "10.0.0.1", epoch, g2));
        assert!(!manager.validate_session_authority(
            "c1",
            "10.0.0.1",
            epoch,
            g2 + 1
        ));

        // Reassignment opens a NEW window — stale generations from the
        // previous window stay fenced even though they once authorized.
        registry
            .assign_device_with_audit(
                "10.0.0.1",
                Some("c1".to_string()),
                "test",
                "reassign",
            )
            .unwrap();
        manager.resync_collector("c1").await;
        let g3 = rx.recv().await.expect("reassignment sync").generation;
        assert!(g3 > g2);
        assert!(manager.validate_session_authority("c1", "10.0.0.1", epoch, g3));
        assert!(!manager.validate_session_authority("c1", "10.0.0.1", epoch, g1));
        assert!(!manager.validate_session_authority("c1", "10.0.0.1", epoch, g2));
    }

    /// A new CollectorManager over the same registry state mints a
    /// strictly newer authority epoch (persisted high-water), so
    /// lifecycle evidence stamped under the previous epoch is fenced.
    #[tokio::test]
    async fn session_authority_epoch_changes_across_restart() {
        let (manager, registry, dir) =
            manager_with_device(AcquisitionMode::Explicit).await;
        let epoch1 = manager.authority_epoch();
        let (mut rx, _gen) = manager
            .register_collector(
                "c1".to_string(),
                crate::api::PROTOCOL_VERSION_CURRENT,
                true,
            )
            .await
            .unwrap();
        let issued_gen = rx.recv().await.expect("initial sync").generation;

        // Simulated core restart: a fresh manager on the same registry
        // must not reuse the previous epoch.
        let credentials = Arc::new(
            CredentialVault::open(dir.path().join("vault"), "BONSAI_ASSIGNMENT_TEST_VAULT")
                .unwrap(),
        );
        let manager2 = CollectorManager::new(registry, credentials, vec![]);
        let epoch2 = manager2.authority_epoch();
        assert!(epoch2 > epoch1, "restart must mint a newer epoch");
        // Old-epoch evidence is rejected outright.
        assert!(!manager2.validate_session_authority("c1", "10.0.0.1", epoch1, issued_gen));
    }
}

/// Whether a delivered assignment currently carries live acquisition
/// authority on the collector: not rejected, device convertible intent
/// is not disabled, and an explicit selection is not empty. Loss of
/// authority (rejected/disabled/explicit-empty) is a withdrawal of the
/// previous authority even though the address remains assigned.
fn assignment_has_acquisition_authority(assignment: &DeviceAssignment) -> bool {
    if assignment.rejected {
        return false;
    }
    let Some(device) = assignment.device.as_ref() else {
        return false;
    };
    if device.acquisition == crate::config::AcquisitionMode::Disabled.as_str() {
        return false;
    }
    if device.acquisition == crate::config::AcquisitionMode::Explicit.as_str()
        && device.selected_paths.is_empty()
    {
        return false;
    }
    true
}

/// Whether an assignment carries credential material that requires the
/// authenticated collector boundary: resolved login credentials or a
/// client-key reference (private-key material stays collector-local but
/// its use is still credential-bearing).
fn assignment_carries_credentials(assignment: &DeviceAssignment) -> bool {
    !assignment.username.is_empty()
        || !assignment.password.is_empty()
        || assignment
            .device
            .as_ref()
            .is_some_and(|d| !d.client_key.trim().is_empty())
}

/// Build an explicit rejected/unavailable assignment. The device
/// configuration is still delivered so the collector can attribute and
/// report the rejection, but NO credentials are included and the
/// collector must not run a subscriber for it.
fn rejected_assignment(target: &TargetConfig, reject_reason: String) -> DeviceAssignment {
    DeviceAssignment {
        device: Some(crate::api::managed_device_from_target(target)),
        username: String::new(),
        password: String::new(),
        rejected: true,
        reject_reason,
    }
}

/// Create a device assignment, resolving credentials on core.
///
/// A configured `credential_alias` that fails to resolve (missing,
/// locked, revoked, malformed) is an explicit rejected outcome — the
/// assignment is NEVER delivered as an apparently usable anonymous
/// assignment. Certificate-only (mTLS) authentication and explicitly
/// unauthenticated devices remain valid anonymous assignments.
fn try_create_assignment(
    target: &TargetConfig,
    vault: &CredentialVault,
    registry: &ApiRegistry,
) -> std::result::Result<DeviceAssignment, String> {
    let mut username = String::new();
    let mut password = String::new();

    let credential_result: std::result::Result<(), String> = (|| {
        let alias = target
            .credential_alias
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty());
        if let Some(alias) = alias {
            // Required login credential: resolution failure rejects the
            // whole assignment. Error text carries the alias and cause —
            // never secret material.
            let creds = vault
                .resolve(alias, ResolvePurpose::Subscribe)
                .map_err(|e| format!("credential_unavailable: '{alias}' ({e:#})"))?;
            password = creds.password_string();
            username = creds.username;
        } else {
            let resolved_user = target.resolved_username().filter(|u| !u.trim().is_empty());
            let resolved_pass = target.resolved_password().filter(|p| !p.is_empty());
            match (resolved_user, resolved_pass) {
                (Some(u), Some(p)) => {
                    username = u;
                    password = p;
                }
                (None, None) => {}
                // Partial credential metadata must not be silently
                // completed with an empty password.
                _ => {
                    return Err(
                        "credential_incomplete: username and password must both resolve"
                            .to_string(),
                    );
                }
            }
        }
        Ok(())
    })();

    let mut target_clone = target.clone();
    let overrides = registry.list_overrides().unwrap_or_default();
    let (resolved_paths, _) =
        crate::discovery::resolve_subscription_paths(&target_clone, &overrides);
    target_clone.selected_paths = resolved_paths;

    match credential_result {
        Ok(()) => Ok(DeviceAssignment {
            device: Some(crate::api::managed_device_from_target(&target_clone)),
            username,
            password,
            rejected: false,
            reject_reason: String::new(),
        }),
        Err(reject_reason) => Ok(rejected_assignment(&target_clone, reject_reason)),
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

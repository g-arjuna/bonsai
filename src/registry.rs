use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::config::TargetConfig;

const REGISTRY_CHANNEL_CAPACITY: usize = 64;

/// A change to the set of managed devices.
#[derive(Clone, Debug)]
pub enum RegistryChange {
    Added(TargetConfig),
    Removed(String), // address
    Updated(TargetConfig),
}

/// Source of truth for which devices bonsai manages.
///
/// `FileRegistry` wraps the static `bonsai.toml` target list.
/// `ApiRegistry` persists the active managed-device set and emits runtime change events.
pub trait DeviceRegistry: Send + Sync {
    fn list_active(&self) -> anyhow::Result<Vec<TargetConfig>>;
    /// Returns a receiver that yields changes as they occur.
    fn subscribe_changes(&self) -> mpsc::Receiver<RegistryChange>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideScope {
    Site(String),
    RoleEnv { role: String, environment: String },
    Device(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideAction {
    Add,
    Drop,
    Modify,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathOverride {
    pub scope: OverrideScope,
    pub path: String,
    pub action: OverrideAction,
    pub sample_interval_s: Option<u64>,
    pub optional: Option<bool>,
    #[serde(default)]
    pub created_at_ns: i64,
    #[serde(default)]
    pub created_by: String,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct RegistryState {
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
    #[serde(default)]
    overrides: Vec<PathOverride>,
}

impl RegistryState {
    fn from_targets(targets: Vec<TargetConfig>) -> Self {
        let mut by_address = BTreeMap::new();
        for target in targets {
            by_address.insert(target.address.clone(), target);
        }
        Self {
            targets: by_address,
            overrides: Vec::new(),
        }
    }

    fn to_vec(&self) -> Vec<TargetConfig> {
        self.targets.values().cloned().collect()
    }
}

/// Registry backed by the static `[[target]]` list loaded from `bonsai.toml`.
/// Emits `Added` for the initial snapshot so event-driven subscriber managers can
/// preserve today's startup behavior without a separate bootstrap code path.
pub struct FileRegistry {
    targets: Vec<TargetConfig>,
}

impl FileRegistry {
    pub fn new(targets: Vec<TargetConfig>) -> Self {
        Self { targets }
    }
}

impl DeviceRegistry for FileRegistry {
    fn list_active(&self) -> anyhow::Result<Vec<TargetConfig>> {
        Ok(self.targets.clone())
    }

    fn subscribe_changes(&self) -> mpsc::Receiver<RegistryChange> {
        let (tx, rx) = mpsc::channel(self.targets.len().max(1));
        let initial_targets = self.targets.clone();
        tokio::spawn(async move {
            for target in initial_targets {
                if tx.send(RegistryChange::Added(target)).await.is_err() {
                    break;
                }
            }
        });
        rx
    }
}

/// Runtime registry persisted to SQLite (preferred) or JSON file.
///
/// The on-disk state is the durable managed-device set for v1. Devices from
/// `bonsai.toml` are treated as seed entries and merged in on startup so local
/// static targets keep working while API-added devices survive a restart.
pub struct ApiRegistry {
    path: PathBuf,
    state: Arc<Mutex<RegistryState>>,
    change_tx: broadcast::Sender<RegistryChange>,
    sqlite_store: Option<Arc<crate::sqlite_store::SqliteStore>>,
}

impl ApiRegistry {
    pub fn open(path: impl Into<PathBuf>, seed_targets: Vec<TargetConfig>) -> Result<Self> {
        let path = path.into();
        let runtime_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let sqlite_store = match crate::sqlite_store::SqliteStore::open(runtime_dir) {
            Ok(store) => Some(std::sync::Arc::new(store)),
            Err(error) => {
                warn!(
                    %error,
                    "SQLite config store unavailable; falling back to JSON registry persistence"
                );
                None
            }
        };

        // Try to migrate from JSON if SQLite is new
        if let Some(ref store) = sqlite_store {
            let _ = store.migrate_from_json(runtime_dir);
        }

        let state = Self::load_state(
            &path,
            seed_targets,
            sqlite_store.as_ref().map(|s| s.as_ref()),
        )?;
        let (change_tx, _) = broadcast::channel(REGISTRY_CHANNEL_CAPACITY);

        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
            change_tx,
            sqlite_store,
        })
    }

    /// Direct access to the backing SQLite config store, when available.
    /// Used by trusted-identity administration (BN-021b); `None` when the
    /// store failed to open at startup.
    pub fn sqlite_store(&self) -> Option<Arc<crate::sqlite_store::SqliteStore>> {
        self.sqlite_store.clone()
    }

    pub fn get_device(&self, address: &str) -> Result<Option<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state.targets.get(address).cloned())
    }

    /// Find a registered device by its stable external identity
    /// (e.g. `netbox:https:netbox.example.com:443:device:123`). Returns
    /// `None` when no device carries the id. Fails closed when the id is
    /// ambiguous — multiple matches mean the identity contract was
    /// violated and no deterministic winner may be chosen.
    pub fn find_by_external_id(&self, external_id: &str) -> Result<Option<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let matches: Vec<TargetConfig> = state
            .targets
            .values()
            .filter(|t| t.external_id.as_deref() == Some(external_id))
            .cloned()
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            n => Err(anyhow!(
                "external id '{external_id}' is ambiguous: bound to {n} devices; \
                 refusing to pick an arbitrary match"
            )),
        }
    }

    /// Find the registered device occupying a canonical device address
    /// (the bare host/IP used as the graph `Device.address` key). Matches
    /// registry entries stored with or without a gNMI port, so an import
    /// of `10.0.0.1` cannot slip past an existing `10.0.0.1:57400` entry.
    /// Fails closed when the canonical address is ambiguous.
    pub fn find_by_canonical_address(&self, address: &str) -> Result<Option<TargetConfig>> {
        let canonical = canonical_device_key(address);
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let matches: Vec<TargetConfig> = state
            .targets
            .values()
            .filter(|t| canonical_device_key(&t.address) == canonical)
            .cloned()
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            n => Err(anyhow!(
                "canonical address '{canonical}' is ambiguous: claimed by {n} devices; \
                 refusing to pick an arbitrary match"
            )),
        }
    }

    pub fn add_device(&self, target: TargetConfig) -> Result<TargetConfig> {
        self.add_device_with_audit(target, &default_audit_actor(), "registry_add_device")
    }

    pub fn add_device_with_audit(
        &self,
        target: TargetConfig,
        actor: &str,
        action: &str,
    ) -> Result<TargetConfig> {
        let address = normalize_address(&target.address)?;
        let mut target = target;
        target.address = address.clone();
        target.external_id = normalize_external_id(target.external_id)?;
        stamp_new_target_audit(&mut target, actor, action);

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        if state.targets.contains_key(&address) {
            bail!("device '{address}' already exists");
        }
        reject_canonical_conflict(&state, &address)?;
        reject_external_id_conflict(&state, &target)?;
        reject_inventory_enabled(&target)?;

        // Persist BEFORE committing to memory: a durable-write failure
        // must leave both stores untouched and emit no change event.
        let mut new_state = state.clone();
        new_state.targets.insert(address.clone(), target.clone());
        self.persist_committed(
            &self.path,
            &new_state,
            self.sqlite_store.as_deref(),
            |store| store.upsert_device(&target, actor, action),
        )?;
        *state = new_state;
        drop(state);

        let _ = self.change_tx.send(RegistryChange::Added(target.clone()));
        Ok(target)
    }

    pub fn update_device(&self, target: TargetConfig) -> Result<TargetConfig> {
        self.update_device_with_audit(target, &default_audit_actor(), "registry_update_device")
    }

    pub fn update_device_with_audit(
        &self,
        target: TargetConfig,
        actor: &str,
        action: &str,
    ) -> Result<TargetConfig> {
        let address = normalize_address(&target.address)?;
        let mut target = target;
        target.address = address.clone();
        target.external_id = normalize_external_id(target.external_id)?;

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let Some(existing) = state.targets.get(&address).cloned() else {
            bail!("device '{address}' does not exist");
        };
        // External identity is owned by the import that created it;
        // updates that don't carry one must not clear it.
        if target.external_id.is_none() {
            target.external_id = existing.external_id.clone();
        }
        stamp_existing_target_audit(&mut target, &existing, actor, action);

        // BN-015-U1 lifecycle boundary: an inventory-only asset can only
        // leave inventory-only through an explicit collection plan —
        // `enabled = true` together with a non-empty `selected_paths`
        // plan. Enabling without a plan is rejected; a disabled update
        // keeps the asset inventory-only. This also blocks bulk "start",
        // CLI enable and gRPC updates that carry no plan.
        if existing.inventory_only {
            if target.enabled && target.selected_paths.is_empty() {
                bail!(
                    "device '{address}' is inventory-only; enabling collection requires \
                     an explicit collection plan (selected_paths)"
                );
            }
            target.inventory_only = !(target.enabled && !target.selected_paths.is_empty());
        } else {
            // Configured devices keep their lifecycle marker cleared; an
            // explicit inventory_only=true on a configured device is a
            // no-op downgrade attempt and is ignored.
            target.inventory_only = false;
        }

        reject_canonical_conflict(&state, &address)?;
        reject_external_id_conflict(&state, &target)?;

        let mut new_state = state.clone();
        new_state.targets.insert(address.clone(), target.clone());
        self.persist_committed(
            &self.path,
            &new_state,
            self.sqlite_store.as_deref(),
            |store| store.upsert_device(&target, actor, action),
        )?;
        *state = new_state;
        drop(state);

        let _ = self.change_tx.send(RegistryChange::Updated(target.clone()));
        Ok(target)
    }

    pub fn remove_device(&self, address: &str) -> Result<Option<TargetConfig>> {
        let address = normalize_address(address)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let Some(removed) = state.targets.get(&address).cloned() else {
            return Ok(None);
        };

        let mut new_state = state.clone();
        new_state.targets.remove(&address);
        if let Some(store) = self.sqlite_store.as_deref() {
            store
                .delete_device(&address, "registry")
                .context("failed to persist device removal")?;
        } else {
            Self::write_json_state(&self.path, &new_state)?;
        }
        *state = new_state;
        drop(state);

        let _ = self.change_tx.send(RegistryChange::Removed(address));
        Ok(Some(removed))
    }

    pub fn list_overrides(&self) -> Result<Vec<PathOverride>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state.overrides.clone())
    }

    pub fn add_override(&self, ovr: PathOverride) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let mut new_state = state.clone();
        new_state.overrides.push(ovr);
        Self::persist_state(&self.path, &new_state, self.sqlite_store.as_deref())?;
        *state = new_state;
        Ok(())
    }

    pub fn remove_override(&self, scope: &OverrideScope, path: &str) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let len_before = state.overrides.len();
        let mut new_state = state.clone();
        new_state
            .overrides
            .retain(|o| !(o.scope == *scope && o.path == path));
        let changed = new_state.overrides.len() != len_before;
        if changed {
            Self::persist_state(&self.path, &new_state, self.sqlite_store.as_deref())?;
            *state = new_state;
        }
        Ok(changed)
    }

    pub fn list_all_targets(&self) -> Result<Vec<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state.targets.values().cloned().collect())
    }

    pub fn list_assigned_to(&self, collector_id: &str) -> Result<Vec<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state
            .targets
            .values()
            .filter(|t| t.collector_id.as_deref() == Some(collector_id))
            .cloned()
            .collect())
    }

    pub fn assign_device(
        &self,
        address: &str,
        collector_id: Option<String>,
    ) -> Result<TargetConfig> {
        self.assign_device_with_audit(
            address,
            collector_id,
            &default_audit_actor(),
            "registry_assign_device",
        )
    }

    pub fn assign_device_with_audit(
        &self,
        address: &str,
        collector_id: Option<String>,
        actor: &str,
        action: &str,
    ) -> Result<TargetConfig> {
        let address = normalize_address(address)?;
        let target = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            let Some(existing) = state.targets.get(&address).cloned() else {
                bail!("device '{address}' does not exist");
            };
            let mut updated = existing;
            updated.collector_id = collector_id;
            updated.updated_at_ns = now_ns();
            updated.updated_by = actor.to_string();
            updated.last_operator_action = action.to_string();

            let mut new_state = state.clone();
            new_state.targets.insert(address.clone(), updated.clone());
            self.persist_committed(
                &self.path,
                &new_state,
                self.sqlite_store.as_deref(),
                |store| store.upsert_device(&updated, actor, action),
            )?;
            *state = new_state;
            updated
        };

        let _ = self.change_tx.send(RegistryChange::Updated(target.clone()));
        Ok(target)
    }

    fn load_state(
        path: &Path,
        seed_targets: Vec<TargetConfig>,
        sqlite_store: Option<&crate::sqlite_store::SqliteStore>,
    ) -> Result<RegistryState> {
        let mut targets = if let Some(store) = sqlite_store {
            store
                .list_devices()
                .context("failed to load managed devices from SQLite config store")?
        } else if path.exists() {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read registry '{}'", path.display()))?;
            let raw = raw.trim_start();
            if raw.starts_with('[') {
                serde_json::from_str(raw).with_context(|| {
                    format!("failed to parse legacy registry '{}'", path.display())
                })?
            } else {
                serde_json::from_str(raw)
                    .with_context(|| format!("failed to parse registry '{}'", path.display()))?
            }
        } else {
            Vec::new()
        };

        for mut target in seed_targets {
            let address = normalize_address(&target.address)?;
            target.address = address.clone();
            if let Some(existing) = targets.iter_mut().find(|t| t.address == address) {
                merge_seed_target(existing, &target);
            } else {
                targets.push(target);
            }
        }

        let state = RegistryState::from_targets(targets);
        Self::persist_state(path, &state, sqlite_store)?;
        Ok(state)
    }

    /// Persist a committed mutation. With a SQLite store, only the
    /// changed device row is written (with its real actor/action for the
    /// audit trail); without one, the whole registry state is written to
    /// the JSON fallback file. Errors propagate — callers commit the
    /// in-memory state only after this succeeds.
    fn persist_committed(
        &self,
        path: &Path,
        new_state: &RegistryState,
        sqlite_store: Option<&crate::sqlite_store::SqliteStore>,
        upsert: impl FnOnce(&crate::sqlite_store::SqliteStore) -> Result<()>,
    ) -> Result<()> {
        if let Some(store) = sqlite_store {
            upsert(store).context("failed to persist device mutation to SQLite")?;
        } else {
            Self::write_json_state(path, new_state)?;
        }
        Ok(())
    }

    /// Persist the full registry state. Used at load time (seed merge)
    /// and for override changes, which have no per-device SQLite row.
    /// SQLite write failures propagate — startup must not report a
    /// durable registry it failed to write.
    fn persist_state(
        path: &Path,
        state: &RegistryState,
        sqlite_store: Option<&crate::sqlite_store::SqliteStore>,
    ) -> Result<()> {
        if let Some(store) = sqlite_store {
            for target in state.targets.values() {
                store
                    .upsert_device(target, "registry", "persist")
                    .with_context(|| {
                        format!(
                            "failed to persist device '{}' to SQLite config store",
                            target.address
                        )
                    })?;
            }
        } else {
            Self::write_json_state(path, state)?;
        }
        Ok(())
    }

    fn write_json_state(path: &Path, state: &RegistryState) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create registry directory '{}'", parent.display())
        })?;

        let serialized =
            serde_json::to_string_pretty(&state).context("failed to serialize registry state")?;
        std::fs::write(path, serialized)
            .with_context(|| format!("failed to write registry '{}'", path.display()))?;
        Ok(())
    }
}

fn merge_seed_target(existing: &mut TargetConfig, seed: &TargetConfig) {
    // BN-015-U1: an inventory-only asset is authoritative for its own
    // lifecycle. Seed config (`bonsai.toml`) must not accidentally
    // activate it or graft acquisition fields onto it — the explicit
    // inventory→configured transition only happens through a validated
    // collection plan on the mutation API.
    if existing.inventory_only {
        return;
    }
    existing.enabled = seed.enabled;
    if existing
        .tls_domain
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        existing.tls_domain = seed.tls_domain.clone();
    }
    if existing.ca_cert.as_deref().unwrap_or_default().is_empty() {
        existing.ca_cert = seed.ca_cert.clone();
    }
    if existing.vendor.as_deref().unwrap_or_default().is_empty() {
        existing.vendor = seed.vendor.clone();
    }
    if existing
        .credential_alias
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        existing.credential_alias = seed.credential_alias.clone();
    }
    if existing
        .username_env
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        existing.username_env = seed.username_env.clone();
    }
    if existing
        .password_env
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        existing.password_env = seed.password_env.clone();
    }
    if existing.username.as_deref().unwrap_or_default().is_empty() {
        existing.username = seed.username.clone();
    }
    if existing.password.as_deref().unwrap_or_default().is_empty() {
        existing.password = seed.password.clone();
    }
    if existing.hostname.as_deref().unwrap_or_default().is_empty() {
        existing.hostname = seed.hostname.clone();
    }
    if existing.role.as_deref().unwrap_or_default().is_empty() {
        existing.role = seed.role.clone();
    }
    if existing.site.as_deref().unwrap_or_default().is_empty() {
        existing.site = seed.site.clone();
    }
    if existing
        .collector_id
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        existing.collector_id = seed.collector_id.clone();
    }
    if existing.paths.is_empty() {
        existing.paths = seed.paths.clone();
    }
}

fn default_audit_actor() -> String {
    std::env::var("BONSAI_OPERATOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

fn stamp_new_target_audit(target: &mut TargetConfig, actor: &str, action: &str) {
    let now = now_ns();
    if target.created_at_ns <= 0 {
        target.created_at_ns = now;
    }
    target.updated_at_ns = now;
    if target.created_by.trim().is_empty() {
        target.created_by = actor.to_string();
    }
    target.updated_by = actor.to_string();
    target.last_operator_action = action.to_string();
}

fn stamp_existing_target_audit(
    target: &mut TargetConfig,
    existing: &TargetConfig,
    actor: &str,
    action: &str,
) {
    target.created_at_ns = if existing.created_at_ns > 0 {
        existing.created_at_ns
    } else {
        now_ns()
    };
    target.created_by = if existing.created_by.trim().is_empty() {
        actor.to_string()
    } else {
        existing.created_by.clone()
    };
    target.updated_at_ns = now_ns();
    target.updated_by = actor.to_string();
    target.last_operator_action = action.to_string();
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

impl DeviceRegistry for ApiRegistry {
    fn list_active(&self) -> anyhow::Result<Vec<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state.to_vec())
    }

    fn subscribe_changes(&self) -> mpsc::Receiver<RegistryChange> {
        let mut rx = self.change_tx.subscribe();
        let (tx, out_rx) = mpsc::channel(REGISTRY_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(change) => {
                        if tx.send(change).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            skipped,
                            "registry change listener lagged behind broadcast channel"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        out_rx
    }
}

/// Return the host portion of `address`, stripping any trailing `:port`.
/// `"10.0.0.1:57400"` → `"10.0.0.1"`, `"10.0.0.1"` → `"10.0.0.1"`,
/// `"[::1]:57400"` → `"::1"`, `"router.lab"` → `"router.lab"`.
/// This is the canonical Device identity key used throughout the graph.
pub fn strip_port(address: &str) -> &str {
    let s = address.trim();
    if let Some(inner) = s.strip_prefix('[') {
        if let Some(end) = inner.find(']') {
            return &inner[..end];
        }
    }
    if let Some((host, _port)) = s.rsplit_once(':') {
        if !host.contains(':') {
            return host;
        }
    }
    s
}

fn normalize_address(address: &str) -> Result<String> {
    let normalized = address.trim();
    if is_valid_host_port(normalized) || is_valid_host(normalized) {
        Ok(normalized.to_string())
    } else {
        bail!("device address must be host or host:port")
    }
}

/// Canonical identity key for the globally address-keyed graph: the
/// port-stripped host with IP literals normalized and hostnames
/// lowercased (DNS is case-insensitive). Used ONLY for uniqueness
/// comparisons — stored addresses keep their original spelling so graph
/// keys (`primary_ip()`) are unchanged.
pub fn canonical_device_key(address: &str) -> String {
    let host = strip_port(address).trim();
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.to_string();
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return ip.to_string();
    }
    host.to_lowercase()
}

/// Normalize and validate an external source identity (e.g.
/// `netbox:https:netbox.example.com:443:device:123`). Identities are
/// namespaced `<source>:<scope>:<kind>:<id>`-style strings; they must be
/// non-empty, reasonably sized and free of whitespace/control characters.
/// Never contains credentials or tokens.
pub fn normalize_external_id(external_id: Option<String>) -> Result<Option<String>> {
    let Some(raw) = external_id else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 512
        || !trimmed.contains(':')
        || trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("external id must be a non-empty namespaced identifier (<=512 bytes, no whitespace)");
    }
    Ok(Some(trimmed.to_string()))
}

/// The graph keys `Device` nodes by canonical (port-stripped) address,
/// so two registry entries that differ only by gNMI port are
/// indistinguishable downstream. Reject the second one explicitly.
fn reject_canonical_conflict(state: &RegistryState, address: &str) -> Result<()> {
    let canonical = canonical_device_key(address);
    if let Some(existing) = state
        .targets
        .values()
        .find(|t| t.address != address && canonical_device_key(&t.address) == canonical)
    {
        bail!(
            "device address '{address}' conflicts with existing device '{}' \
             (same canonical address; the graph cannot distinguish them)",
            existing.address
        );
    }
    Ok(())
}

fn reject_external_id_conflict(state: &RegistryState, target: &TargetConfig) -> Result<()> {
    let Some(external_id) = target.external_id.as_deref() else {
        return Ok(());
    };
    let conflicts: Vec<String> = state
        .targets
        .values()
        .filter(|t| t.address != target.address && t.external_id.as_deref() == Some(external_id))
        .map(|t| t.address.clone())
        .collect();
    if !conflicts.is_empty() {
        bail!(
            "external id '{external_id}' is already bound to {}",
            conflicts.join(", ")
        );
    }
    Ok(())
}

fn reject_inventory_enabled(target: &TargetConfig) -> Result<()> {
    if target.inventory_only && target.enabled {
        bail!(
            "device '{}' cannot be created inventory-only and enabled; \
             activation requires an explicit collection plan",
            target.address
        );
    }
    Ok(())
}

fn is_valid_host_port(address: &str) -> bool {
    if address.is_empty() {
        return false;
    }

    let Some((host, port)) = split_host_port(address) else {
        return false;
    };

    is_valid_port(port) && is_valid_host(host)
}

fn split_host_port(address: &str) -> Option<(&str, &str)> {
    if let Some(rest) = address.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        let remainder = &rest[end + 1..];
        let port = remainder.strip_prefix(':')?;
        if port.contains(':') {
            return None;
        }
        return Some((host, port));
    }

    let (host, port) = address.rsplit_once(':')?;
    if host.contains(':') || port.contains(':') {
        return None;
    }
    Some((host, port))
}

fn is_valid_port(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|p| p > 0)
}

fn is_valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.contains(char::is_whitespace) {
        return false;
    }

    if host.parse::<Ipv4Addr>().is_ok() {
        return true;
    }

    if host.parse::<Ipv6Addr>().is_ok() {
        return true;
    }

    host.split('.').all(is_valid_hostname_label)
}

fn is_valid_hostname_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }

    let bytes = label.as_bytes();
    let first = bytes[0] as char;
    let last = bytes[bytes.len() - 1] as char;
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return false;
    }

    label
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry_dir(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("bonsai-{name}-"))
            .tempdir()
            .expect("tempdir")
    }

    fn target(address: &str, vendor: &str) -> TargetConfig {
        TargetConfig {
            extra_ips: Vec::new(),
            optional: false,
            paths: Vec::new(),
            address: address.to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some(vendor.to_string()),
            credential_alias: None,
            username_env: Some("BONSAI_TEST_USER".to_string()),
            password_env: Some("BONSAI_TEST_PASS".to_string()),
            username: None,
            password: None,
            hostname: Some(format!("{vendor}-host")),
            role: Some("leaf".to_string()),
            site: Some("lab".to_string()),
            collector_id: None,
            selected_paths: Vec::new(),
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
            external_id: None,
            inventory_only: false,
            inventory_platform: None,
            inventory_status: None,
            inventory_tenant: None,
        }
    }

    #[test]
    fn normalize_address_accepts_host_port_forms() {
        for address in [
            "10.0.0.1:57400",
            "leaf-1.lab.local:57400",
            "localhost:50051",
            "[2001:db8::1]:57400",
            "10.0.0.1",
            "leaf-1.lab.local",
            "localhost",
            "router-rr",
        ] {
            assert_eq!(normalize_address(address).unwrap(), address);
        }
        assert_eq!(
            normalize_address("  leaf-1.lab.local:57400  ").unwrap(),
            "leaf-1.lab.local:57400"
        );
        assert_eq!(
            normalize_address("  leaf-1.lab.local  ").unwrap(),
            "leaf-1.lab.local"
        );
    }

    #[test]
    fn normalize_address_rejects_invalid_forms() {
        for address in [
            "",
            "10.0.0.1:0",
            ":57400",
            "bad host:57400",
            "2001:db8::1:57400",
            "leaf-.lab:57400",
            "-leaf.lab:57400",
            "leaf:70000",
        ] {
            let error = normalize_address(address).unwrap_err().to_string();
            assert!(
                error.contains("device address must be host or host:port"),
                "unexpected error for {address:?}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn api_registry_persists_and_emits_changes() {
        let dir = test_registry_dir("registry");
        let path = dir.path().join("registry.json");
        let registry = ApiRegistry::open(&path, vec![target("10.0.0.1:57400", "seed")])
            .expect("open registry");

        let initial = registry.list_active().expect("list initial");
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].address, "10.0.0.1:57400");

        let mut changes = registry.subscribe_changes();

        let added = registry
            .add_device(target("10.0.0.2:57400", "nokia_srl"))
            .expect("add device");
        assert_eq!(added.address, "10.0.0.2:57400");

        match changes.recv().await.expect("added change") {
            RegistryChange::Added(target) => assert_eq!(target.address, "10.0.0.2:57400"),
            other => panic!("expected Added, got {other:?}"),
        }

        let mut updated = target("10.0.0.2:57400", "cisco_xrd");
        updated.role = Some("spine".to_string());
        registry
            .update_device(updated.clone())
            .expect("update device");

        match changes.recv().await.expect("updated change") {
            RegistryChange::Updated(target) => {
                assert_eq!(target.address, updated.address);
                assert_eq!(target.role, Some("spine".to_string()));
            }
            other => panic!("expected Updated, got {other:?}"),
        }

        let removed = registry
            .remove_device("10.0.0.2:57400")
            .expect("remove device");
        assert!(removed.is_some());

        match changes.recv().await.expect("removed change") {
            RegistryChange::Removed(address) => assert_eq!(address, "10.0.0.2:57400"),
            other => panic!("expected Removed, got {other:?}"),
        }

        let reopened = ApiRegistry::open(&path, vec![]).expect("reopen registry");
        let active = reopened.list_active().expect("list reopened");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].address, "10.0.0.1:57400");
    }

    fn inventory_target(address: &str, external_id: &str) -> TargetConfig {
        let mut t = target(address, "inventory");
        t.enabled = false;
        t.inventory_only = true;
        t.external_id = Some(external_id.to_string());
        t.credential_alias = None;
        t.username_env = None;
        t.password_env = None;
        t.username = None;
        t.password = None;
        t.selected_paths = Vec::new();
        t
    }

    #[test]
    fn external_id_is_unique_per_device() {
        let dir = test_registry_dir("extid");
        let registry = ApiRegistry::open(dir.path().join("registry.json"), vec![])
            .expect("open registry");

        registry
            .add_device(inventory_target("10.30.0.1", "netbox:https:nb1.example.com:443:device:123"))
            .expect("first instance import");
        // A second NetBox instance may legitimately have device id 123 —
        // its instance-scoped identity is distinct and must not collide.
        registry
            .add_device(inventory_target("10.30.0.2", "netbox:https:nb2.example.com:443:device:123"))
            .expect("second instance import");
        // The SAME instance-scoped identity cannot bind twice.
        let dup = registry.add_device(inventory_target(
            "10.30.0.3",
            "netbox:https:nb1.example.com:443:device:123",
        ));
        assert!(dup.is_err(), "duplicate external id must be rejected");

        assert!(registry
            .find_by_external_id("netbox:https:nb1.example.com:443:device:123")
            .expect("lookup")
            .is_some());
        assert!(registry
            .find_by_external_id("netbox:https:nb2.example.com:443:device:123")
            .expect("lookup")
            .is_some());
    }

    #[test]
    fn ambiguous_external_id_fails_closed() {
        let dir = test_registry_dir("ambiguous");
        let registry = ApiRegistry::open(dir.path().join("registry.json"), vec![])
            .expect("open registry");
        registry
            .add_device(inventory_target("10.30.1.1", "src:a:device:1"))
            .expect("add first");
        // Simulate pre-existing ambiguity that predates the uniqueness
        // enforcement (e.g. hand-edited state): the lookup must error
        // rather than pick an arbitrary winner.
        {
            let mut state = registry.state.lock().expect("lock");
            let mut impostor = inventory_target("10.30.1.2", "src:a:device:1");
            impostor.address = "10.30.1.2".to_string();
            state.targets.insert("10.30.1.2".to_string(), impostor);
        }
        let err = registry
            .find_by_external_id("src:a:device:1")
            .expect_err("ambiguous external id must fail closed");
        assert!(err.to_string().contains("ambiguous"), "{err}");
    }

    #[test]
    fn canonical_address_rejects_port_variants() {
        let dir = test_registry_dir("canonical");
        let registry = ApiRegistry::open(dir.path().join("registry.json"), vec![])
            .expect("open registry");
        registry
            .add_device(target("10.30.2.1:57400", "gnmi"))
            .expect("add host:port device");
        // The graph keys Device nodes by port-stripped address — a bare
        // import of the same host must not create a second entry.
        let bare = registry.add_device(inventory_target("10.30.2.1", "src:a:device:9"));
        assert!(bare.is_err(), "bare address must conflict with host:port entry");
        // ...and the canonical lookup finds the port-qualified entry.
        let found = registry
            .find_by_canonical_address("10.30.2.1")
            .expect("lookup")
            .expect("canonical match");
        assert_eq!(found.address, "10.30.2.1:57400");
    }

    #[test]
    fn ipv6_canonical_key_normalizes() {
        assert_eq!(canonical_device_key("[2001:db8::1]:57400"), "2001:db8::1");
        assert_eq!(canonical_device_key("2001:db8::1"), "2001:db8::1");
        assert_eq!(canonical_device_key("2001:0DB8::1"), "2001:db8::1");
        assert_eq!(canonical_device_key("Router-1"), "router-1");
        assert_eq!(canonical_device_key("10.0.0.1:57400"), "10.0.0.1");
    }

    #[test]
    fn normalize_external_id_validates_shape() {
        assert_eq!(
            normalize_external_id(Some(" netbox:https:nb:443:device:1 ".to_string()))
                .expect("normalize")
                .as_deref(),
            Some("netbox:https:nb:443:device:1")
        );
        assert_eq!(normalize_external_id(None).expect("none"), None);
        assert_eq!(
            normalize_external_id(Some("   ".to_string())).expect("blank"),
            None
        );
        for bad in ["no-colon", "has space:x", "tab\there:x"] {
            assert!(
                normalize_external_id(Some(bad.to_string())).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn inventory_only_requires_plan_for_activation() {
        let dir = test_registry_dir("lifecycle");
        let registry = ApiRegistry::open(dir.path().join("registry.json"), vec![])
            .expect("open registry");
        registry
            .add_device(inventory_target("10.30.3.1", "src:a:device:1"))
            .expect("import inventory asset");

        // Metadata-only update stays inventory-only.
        let mut meta = inventory_target("10.30.3.1", "src:a:device:1");
        meta.hostname = Some("renamed".to_string());
        let updated = registry
            .update_device_with_audit(meta, "api", "api_update")
            .expect("metadata update");
        assert!(updated.inventory_only);
        assert!(!updated.enabled);

        // Enable WITHOUT a collection plan → rejected.
        let mut enable = registry.get_device("10.30.3.1").expect("get").expect("exists");
        enable.enabled = true;
        let err = registry
            .update_device_with_audit(enable, "api", "api_enable")
            .expect_err("enable without plan must fail");
        assert!(err.to_string().contains("collection plan"), "{err}");
        assert!(registry
            .get_device("10.30.3.1")
            .expect("get")
            .expect("exists")
            .inventory_only);

        // Enable WITH an explicit collection plan → explicit activation,
        // external identity preserved.
        let mut activate = registry.get_device("10.30.3.1").expect("get").expect("exists");
        activate.enabled = true;
        activate.selected_paths = vec![crate::config::SelectedSubscriptionPath {
            path: "/interfaces/interface/state/counters".to_string(),
            origin: "test".to_string(),
            mode: "poll".to_string(),
            sample_interval_ns: 0,
            rationale: String::new(),
            optional: false,
        }];
        let activated = registry
            .update_device_with_audit(activate, "api", "api_activate")
            .expect("activation with plan");
        assert!(!activated.inventory_only);
        assert!(activated.enabled);
        assert_eq!(
            activated.external_id.as_deref(),
            Some("src:a:device:1")
        );

        // Identity survives a later gRPC-style update that carries no
        // external_id (callers pass None → inherited, never cleared).
        let mut plain = activated.clone();
        plain.external_id = None;
        plain.hostname = Some("post-activation".to_string());
        let kept = registry
            .update_device_with_audit(plain, "grpc", "grpc_update")
            .expect("update");
        assert_eq!(kept.external_id.as_deref(), Some("src:a:device:1"));
    }

    #[tokio::test]
    async fn failed_persistence_leaves_memory_untouched() {
        // Force the JSON fallback (sqlite unavailable because
        // bonsai_config.db is a directory), then make the registry file
        // read-only after open so the durable write fails.
        let dir = test_registry_dir("blocked");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(runtime.join("bonsai_config.db")).expect("db blocker dir");
        let path = runtime.join("registry.json");
        let registry = ApiRegistry::open(&path, vec![]).expect("open registry");
        assert!(registry.sqlite_store().is_none(), "json fallback expected");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
                .expect("chmod registry read-only");
        }

        let err = registry
            .add_device(target("10.30.4.1", "x"))
            .expect_err("unwritable registry path must fail");
        assert!(format!("{err:#}").contains("registry"), "{err:#}");
        assert!(registry
            .get_device("10.30.4.1")
            .expect("get")
            .is_none());
        assert!(registry.list_active().expect("list").is_empty());
    }

    #[tokio::test]
    async fn sqlite_write_failure_propagates() {
        let dir = test_registry_dir("sqlitefail");
        // Pre-create the config DB with triggers that reject device writes.
        {
            let _store = crate::sqlite_store::SqliteStore::open(dir.path())
                .expect("open store");
            let conn = rusqlite::Connection::open(dir.path().join("bonsai_config.db"))
                .expect("second connection");
            conn.execute_batch(
                "CREATE TRIGGER fail_device_insert BEFORE INSERT ON devices \
                 BEGIN SELECT RAISE(FAIL, 'injected insert failure'); END;\
                 CREATE TRIGGER fail_device_update BEFORE UPDATE ON devices \
                 BEGIN SELECT RAISE(FAIL, 'injected update failure'); END;",
            )
            .expect("install triggers");
        }
        let registry = ApiRegistry::open(dir.path().join("registry.json"), vec![])
            .expect("open registry");
        assert!(registry.sqlite_store().is_some(), "sqlite store expected");

        let mut changes = registry.subscribe_changes();
        let err = registry
            .add_device(target("10.30.5.1", "x"))
            .expect_err("sqlite write failure must propagate");
        assert!(format!("{err:#}").contains("persist"), "{err:#}");
        assert!(registry
            .get_device("10.30.5.1")
            .expect("get")
            .is_none());
        assert!(changes.try_recv().is_err(), "no change event for failed write");
    }
}

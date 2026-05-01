use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Default, Serialize, Deserialize)]
struct RegistryState {
    #[serde(default)]
    targets: BTreeMap<String, TargetConfig>,
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

/// Runtime registry persisted to a local JSON file.
///
/// The on-disk state is the durable managed-device set for v1. Devices from
/// `bonsai.toml` are treated as seed entries and merged in on startup so local
/// static targets keep working while API-added devices survive a restart.
pub struct ApiRegistry {
    path: PathBuf,
    state: Arc<Mutex<RegistryState>>,
    change_tx: broadcast::Sender<RegistryChange>,
}

impl ApiRegistry {
    pub fn open(path: impl Into<PathBuf>, seed_targets: Vec<TargetConfig>) -> Result<Self> {
        let path = path.into();
        let state = Self::load_state(&path, seed_targets)?;
        let (change_tx, _) = broadcast::channel(REGISTRY_CHANNEL_CAPACITY);

        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
            change_tx,
        })
    }

    pub fn get_device(&self, address: &str) -> Result<Option<TargetConfig>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(state.targets.get(address).cloned())
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
        stamp_new_target_audit(&mut target, actor, action);

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            if state.targets.contains_key(&address) {
                bail!("device '{address}' already exists");
            }
            state.targets.insert(address.clone(), target.clone());
            Self::persist_state(&self.path, &state)?;
        }

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

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            let existing = state.targets.get(&address).cloned();
            if existing.is_none() {
                bail!("device '{address}' does not exist");
            }
            if let Some(existing) = existing {
                stamp_existing_target_audit(&mut target, &existing, actor, action);
            }
            state.targets.insert(address.clone(), target.clone());
            Self::persist_state(&self.path, &state)?;
        }

        let _ = self.change_tx.send(RegistryChange::Updated(target.clone()));
        Ok(target)
    }

    pub fn remove_device(&self, address: &str) -> Result<Option<TargetConfig>> {
        let address = normalize_address(address)?;
        let removed = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            let removed = state.targets.remove(&address);
            if removed.is_some() {
                Self::persist_state(&self.path, &state)?;
            }
            removed
        };

        if removed.is_some() {
            let _ = self.change_tx.send(RegistryChange::Removed(address));
        }

        Ok(removed)
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
        state.overrides.push(ovr);
        Self::persist_state(&self.path, &state)?;
        Ok(())
    }

    pub fn remove_override(&self, scope: &OverrideScope, path: &str) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let len_before = state.overrides.len();
        state.overrides.retain(|o| !(o.scope == *scope && o.path == path));
        let changed = state.overrides.len() != len_before;
        if changed {
            Self::persist_state(&self.path, &state)?;
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

    pub fn assign_device(&self, address: &str, collector_id: Option<String>) -> Result<TargetConfig> {
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
                let target = state
                    .targets
                    .get_mut(&address)
                    .ok_or_else(|| anyhow!("device '{address}' does not exist"))?;
                target.collector_id = collector_id;
                target.updated_at_ns = now_ns();
                target.updated_by = actor.to_string();
                target.last_operator_action = action.to_string();
                let updated = target.clone();
                Self::persist_state(&self.path, &state)?;
                updated
        };

        let _ = self.change_tx.send(RegistryChange::Updated(target.clone()));
        Ok(target)
    }

    fn load_state(path: &Path, seed_targets: Vec<TargetConfig>) -> Result<RegistryState> {
        let mut state = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read registry '{}'", path.display()))?;
            let raw = raw.trim_start();
            if raw.starts_with('[') {
                let targets: Vec<TargetConfig> = serde_json::from_str(raw)
                    .with_context(|| format!("failed to parse legacy registry '{}'", path.display()))?;
                RegistryState::from_targets(targets)
            } else {
                serde_json::from_str(raw)
                    .with_context(|| format!("failed to parse registry '{}'", path.display()))?
            }
        } else {
            RegistryState::default()
        };

        for mut target in seed_targets {
            let address = normalize_address(&target.address)?;
            target.address = address.clone();
            state.targets.entry(address).or_insert(target);
        }

        Self::persist_state(path, &state)?;
        Ok(state)
    }

    fn persist_state(path: &Path, state: &RegistryState) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create registry directory '{}'", parent.display())
        })?;

        let serialized = serde_json::to_string_pretty(&state)
            .context("failed to serialize registry state")?;
        std::fs::write(path, serialized)
            .with_context(|| format!("failed to write registry '{}'", path.display()))?;
        Ok(())
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

fn normalize_address(address: &str) -> Result<String> {
    let normalized = address.trim();
    if is_valid_host_port(normalized) {
        Ok(normalized.to_string())
    } else {
        bail!("device address must be host:port")
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_registry_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("bonsai-{name}-{nanos}.json"))
    }

    fn target(address: &str, vendor: &str) -> TargetConfig {
        TargetConfig {
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
        }
    }

    #[test]
    fn normalize_address_accepts_host_port_forms() {
        for address in [
            "10.0.0.1:57400",
            "leaf-1.lab.local:57400",
            "localhost:50051",
            "[2001:db8::1]:57400",
        ] {
            assert_eq!(normalize_address(address).unwrap(), address);
        }
        assert_eq!(
            normalize_address("  leaf-1.lab.local:57400  ").unwrap(),
            "leaf-1.lab.local:57400"
        );
    }

    #[test]
    fn normalize_address_rejects_invalid_forms() {
        for address in [
            "",
            "garbage",
            "10.0.0.1",
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
                error.contains("device address must be host:port"),
                "unexpected error for {address:?}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn api_registry_persists_and_emits_changes() {
        let path = test_registry_path("registry");
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

        let _ = std::fs::remove_file(path);
    }
}

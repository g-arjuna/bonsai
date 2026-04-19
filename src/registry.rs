use tokio::sync::mpsc;

use crate::config::TargetConfig;

/// A change to the set of managed devices.
pub enum RegistryChange {
    Added(TargetConfig),
    Removed(String),        // address
    Updated(TargetConfig),
}

/// Source of truth for which devices bonsai manages.
///
/// Today only `FileRegistry` exists (wraps the static `bonsai.toml` target list).
/// Future implementations: `ApiRegistry` (gRPC AddDevice/RemoveDevice), and
/// `NautobotRegistry` / `NetBoxRegistry` (sync from external source of truth).
pub trait DeviceRegistry: Send + Sync {
    fn list_active(&self) -> anyhow::Result<Vec<TargetConfig>>;
    /// Returns a receiver that yields changes as they occur.
    /// `FileRegistry` returns a channel that never yields (no file-watching yet).
    /// File-watching via the `notify` crate is Phase 4.5 work.
    fn subscribe_changes(&self) -> mpsc::Receiver<RegistryChange>;
}

/// Registry backed by the static `[[target]]` list loaded from `bonsai.toml`.
/// No dynamic onboarding — restart required to add/remove devices.
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
        // No file-watching yet; the returned channel never yields.
        // When `notify` file-watching is added, this will emit Added/Removed/Updated
        // events as bonsai.toml changes on disk.
        let (_, rx) = mpsc::channel(1);
        rx
    }
}

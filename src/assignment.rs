use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use anyhow::Result;
use tokio::sync::{mpsc, broadcast};
use tracing::{info, warn};

use crate::api::pb::{AssignmentUpdate, DeviceAssignment};
use crate::registry::{ApiRegistry, DeviceRegistry, RegistryChange};
use crate::credentials::CredentialVault;

pub struct CollectorManager {
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    active_collectors: Arc<Mutex<HashMap<String, mpsc::Sender<AssignmentUpdate>>>>,
}

impl CollectorManager {
    pub fn new(registry: Arc<ApiRegistry>, credentials: Arc<CredentialVault>) -> Self {
        let manager = Self {
            registry,
            credentials,
            active_collectors: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.start_registry_watcher();
        manager
    }

    fn start_registry_watcher(&self) {
        let registry = self.registry.clone();
        let credentials = self.credentials.clone();
        let active_collectors = self.active_collectors.clone();

        tokio::spawn(async move {
            let mut changes = registry.subscribe_changes();
            while let Some(change) = changes.recv().await {
                match change {
                    RegistryChange::Added(target) | RegistryChange::Updated(target) => {
                        if let Some(collector_id) = &target.collector_id {
                            let tx = {
                                let collectors = active_collectors.lock().unwrap();
                                collectors.get(collector_id).cloned()
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
                    RegistryChange::Removed(_address) => {
                        // For removal, we currently rely on collectors receiving a full sync
                        // or we could add a 'Remove' command to AssignmentUpdate.
                        // For v1, full sync on registration is the primary mechanism.
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
        
        // 1. Send initial full sync
        let targets = self.registry.list_assigned_to(&collector_id)?;
        let mut assignments = Vec::new();
        for target in targets {
            assignments.push(create_assignment(&target, &self.credentials));
        }
        
        tx.send(AssignmentUpdate {
            assignments,
            is_full_sync: true,
        }).await?;

        // 2. Track as active
        {
            let mut collectors = self.active_collectors.lock().unwrap();
            collectors.insert(collector_id.clone(), tx);
        }
        
        info!(%collector_id, "collector registered and initial assignments sent");
        Ok(rx)
    }

    pub fn unregister_collector(&self, collector_id: &str) {
        let mut collectors = self.active_collectors.lock().unwrap();
        collectors.remove(collector_id);
        info!(%collector_id, "collector unregistered");
    }
}

fn create_assignment(target: &crate::config::TargetConfig, vault: &CredentialVault) -> DeviceAssignment {
    let mut username = String::new();
    let mut password = String::new();
    
    // Resolve credentials on core
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

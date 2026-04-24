use anyhow::Result;
use lbug::Database;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::graph::BonsaiEvent;
use crate::telemetry::TelemetryUpdate;

/// Shared interface for GraphStore (core) and CollectorGraphStore (collector).
#[tonic::async_trait]
pub trait BonsaiStore: Send + Sync {
    fn db(&self) -> Arc<Database>;
    fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent>;
    
    async fn write(&self, update: TelemetryUpdate) -> Result<()>;

    async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        fired_at_ns: i64,
        state_change_event_id: String,
    ) -> Result<String>;

    async fn write_remediation(
        &self,
        detection_id: String,
        action: String,
        status: String,
        detail_json: String,
        attempted_at_ns: i64,
        completed_at_ns: i64,
    ) -> Result<String>;

    async fn sync_sites_from_targets(&self, targets: Vec<crate::config::TargetConfig>) -> Result<()>;
    async fn list_sites(&self) -> Result<Vec<crate::graph::SiteRecord>>;
    async fn upsert_site(&self, site: crate::graph::SiteRecord) -> Result<crate::graph::SiteRecord>;

    async fn write_subscription_status(
        &self,
        status: crate::graph::SubscriptionStatusWrite,
    ) -> Result<()>;

    fn publish_event(&self, event: BonsaiEvent);
}

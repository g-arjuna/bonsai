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
    /// KuzuDB allows only one concurrent write transaction. Callers that open a
    /// lbug Connection directly (e.g. enrichers) must hold this lock for the
    /// duration of their writes so they don't race the write coordinator.
    fn write_lock(&self) -> Arc<std::sync::Mutex<()>>;
    fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent>;

    async fn write(&self, update: TelemetryUpdate) -> Result<()>;

    async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        source_types_json: String,
        latency_ns: i64,
        fired_at_ns: i64,
        state_change_event_id: String,
        source_event_ids: Vec<String>,
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

    async fn sync_sites_from_targets(
        &self,
        targets: Vec<crate::config::TargetConfig>,
    ) -> Result<()>;
    async fn list_sites(&self) -> Result<Vec<crate::graph::SiteRecord>>;
    async fn upsert_site(&self, site: crate::graph::SiteRecord)
    -> Result<crate::graph::SiteRecord>;

    async fn write_subscription_status(
        &self,
        status: crate::graph::SubscriptionStatusWrite,
    ) -> Result<()>;

    /// Mark persisted subscription statuses for `device_address` that are
    /// NOT in `keep` (keyed by path/origin/mode/sample_interval_ns) as
    /// "superseded". Called when a new acquisition plan registers so rows
    /// left behind by an obsolete plan — including across restarts, where
    /// the verifier's in-memory tracking is empty — cannot remain
    /// displayed as current. Stores without persistent status read access
    /// may use the default no-op.
    async fn supersede_subscription_statuses_outside_plan(
        &self,
        _device_address: &str,
        _keep: &std::collections::HashSet<(String, String, String, i64)>,
    ) -> Result<()> {
        Ok(())
    }

    /// Expire persisted subscription statuses that claim current
    /// coverage (`observed`, `deleted`, `pending`,
    /// `subscribed_but_silent`) to `stale` at verifier startup. Rows
    /// describe the PREVIOUS boot's sessions — nothing is current until
    /// new verified evidence arrives (M5.2B review B5). `superseded`
    /// rows are already non-current and are left untouched. Stores
    /// without persistent status read access may use the default no-op.
    async fn expire_subscription_statuses_on_boot(&self) -> Result<()> {
        Ok(())
    }

    fn publish_event(&self, event: BonsaiEvent);
}

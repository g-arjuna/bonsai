use axum::response::IntoResponse;
/// Phase 6 HTTP API + SSE server (Axum).
///
/// Runs on port 3000 alongside the Tonic gRPC server (port 50051).
/// Shares the same Arc<GraphStore> — handlers call GraphStore read methods
/// directly, with zero extra serialization vs the gRPC path.
///
/// Endpoints:
///   GET /api/topology          — devices, LLDP links, BGP sessions, health
///   GET /api/detections        — recent DetectionEvents + Remediations
///   GET /api/trace/:id         — closed-loop trace for one DetectionEvent
///   GET /api/events            — SSE stream of live BonsaiEvents
///   GET / (and assets/*)       — Svelte SPA static files from ui/dist/
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures::stream::{Stream, StreamExt};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::assignment::{CollectorManager, CollectorStatus};
use crate::catalogue::CatalogueState;
use crate::enrichment::{EnricherConfig, SharedEnricherRegistry};
use crate::gnmi_set::gnmi_set;
use crate::graph::{
    DetectionRow, EnvironmentRecord, GraphStore, REMEDIATION_TRUST_CUTOFF_ISO,
    RemediationProposalRow, SiteRecord, TraceStep,
};
use crate::output::traits::{OutputAdapterConfig, OutputAdapterRunState, SharedAdapterRegistry};
use crate::resource_governor::GovernorHandle;
use crate::{
    archive, audit,
    change_detection::{self, ChangeDetectionRuntime},
    config::{
        AssignmentRule, LayeredIngestionConfig, RemediationConfig, SelectedSubscriptionPath,
        ServiceNowConfig, StorageConfig, StreamingConfig, TargetConfig,
    },
    credentials::{CredentialSummary, CredentialVault, ResolvePurpose, ResolvedCredential},
    discovery::{self, DiscoveryInput},
    disk_guard, event_bus, memory_profile,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    remediation::{
        SharedRollbackRegistry, SharedTrustStore, TrustKey, TrustState, check_graduation,
    },
    signals::syslog::{SyslogEvent, SyslogFact},
    store::BonsaiStore,
    streaming::{self, StreamingReadinessReport},
    synthesizer,
    yang::YangLibrary,
};


pub(crate) mod types;
#[allow(unused_imports)]
pub(crate) use types::*;

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<GraphStore>,
    pub registry: Arc<ApiRegistry>,
    pub credentials: Arc<CredentialVault>,
    pub change_detection: Arc<ChangeDetectionRuntime>,
    pub collector_manager: Option<Arc<CollectorManager>>,
    pub catalogue: Arc<RwLock<CatalogueState>>,
    pub catalogue_dir: String,
    pub enricher_registry: SharedEnricherRegistry,
    pub adapter_registry: SharedAdapterRegistry,
    pub trust_store: SharedTrustStore,
    pub rollback_registry: SharedRollbackRegistry,
    pub remediation_config: RemediationConfig,
    pub servicenow_config: ServiceNowConfig,
    pub runtime_dir: String,
    pub archive_path: String,
    pub graph_path: String,
    pub storage_config: StorageConfig,
    pub layered_ingestion: LayeredIngestionConfig,
    pub streaming: StreamingConfig,
    pub yang_library_root: String,
    pub yang_cache_root: String,
    pub yang_bundle_key_env: String,
    /// Counter ingest mode for operations visibility (C-9 / T1-8).
    pub counter_mode: String,
    pub counter_window_secs: u64,
    pub counter_debounce_secs: u64,
    /// T4-5: Resource governance handle — None until governor is started (non-core modes).
    pub governor: Option<GovernorHandle>,
    /// CV7 T4-2/T4-4: Sidecar registry. Shared with the gRPC service so both
    /// surfaces see the same data. See `src/sidecar_registry.rs`.
    pub sidecar_registry: Arc<crate::sidecar_registry::SidecarRegistry>,
}

// ── Router ────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn router(
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    change_detection: Arc<ChangeDetectionRuntime>,
    collector_manager: Option<Arc<CollectorManager>>,
    catalogue: Arc<RwLock<CatalogueState>>,
    catalogue_dir: String,
    enricher_registry: SharedEnricherRegistry,
    adapter_registry: SharedAdapterRegistry,
    trust_store: SharedTrustStore,
    rollback_registry: SharedRollbackRegistry,
    remediation_config: RemediationConfig,
    servicenow_config: ServiceNowConfig,
    runtime_dir: String,
    archive_path: String,
    graph_path: String,
    storage_config: StorageConfig,
    layered_ingestion: LayeredIngestionConfig,
    streaming: StreamingConfig,
    yang_library_root: String,
    yang_cache_root: String,
    yang_bundle_key_env: String,
    counter_mode: String,
    counter_window_secs: u64,
    counter_debounce_secs: u64,
    governor: Option<GovernorHandle>,
    sidecar_registry: Arc<crate::sidecar_registry::SidecarRegistry>,
) -> Router {
    let state = AppState {
        store,
        registry,
        credentials,
        change_detection,
        collector_manager,
        catalogue,
        catalogue_dir,
        enricher_registry,
        adapter_registry,
        trust_store,
        rollback_registry,
        remediation_config,
        servicenow_config,
        runtime_dir,
        archive_path,
        graph_path,
        storage_config,
        layered_ingestion,
        streaming,
        yang_library_root,
        yang_cache_root,
        yang_bundle_key_env,
        counter_mode,
        counter_window_secs,
        counter_debounce_secs,
        governor,
        sidecar_registry,
    };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    // CV7 T4-5: Bonpy — a separate Svelte SPA for Python/ML/AIOps surfaces,
    // mounted at /bonpy/ on the same Axum process. Distinct from bonsai UI;
    // see docs/architecture/sidecars.md. If `ui-bonpy/dist/` is missing (e.g.
    // a build that skipped the bonpy step), ServeDir returns 404 — bonsai UI
    // still works. Index fallback enables client-side routing within bonpy.
    let bonpy_spa = ServeDir::new("ui-bonpy/dist")
        .not_found_service(tower_http::services::ServeFile::new(
            "ui-bonpy/dist/index.html",
        ));

    Router::new()
        .route("/api/topology", get(topology_handler))
        .route("/api/yang/modules", get(yang_modules_handler))
        .route("/api/yang/search", get(yang_search_handler))
        .route("/api/overrides", get(list_overrides).post(add_override))
        .route("/api/overrides/remove", post(remove_override))
        .route("/api/path", get(path_handler))
        .route("/api/blast-radius/{address}", get(blast_radius_handler))
        .route("/api/incidents/grouped", get(incidents_handler))
        .route(
            "/api/devices/{address}/gnmi-readiness",
            get(device_gnmi_readiness_handler),
        )
        .route(
            "/api/devices/{address}/streaming-readiness",
            get(device_streaming_readiness_handler),
        )
        .route(
            "/api/devices/{address}/recommendations",
            get(device_recommendations_handler),
        )
        .route(
            "/api/devices/{address}/selected-paths",
            post(apply_device_selected_paths_handler),
        )
        .route(
            "/api/devices/{address}/config-history",
            get(device_config_history_handler),
        )
        .route(
            "/api/devices/{address}/reparse",
            post(device_reparse_handler),
        )
        .route(
            "/api/onboarding/devices",
            get(managed_devices_handler).post(add_managed_device_handler),
        )
        .route(
            "/api/onboarding/devices/with_paths",
            post(add_managed_device_with_paths_handler),
        )
        .route(
            "/api/onboarding/devices/remove",
            post(remove_managed_device_handler),
        )
        .route(
            "/api/onboarding/devices/remove-impact",
            post(remove_impact_handler),
        )
        .route(
            "/api/onboarding/devices/bulk",
            post(bulk_managed_device_action_handler),
        )
        .route("/api/onboarding/discover", post(discover_handler))
        .route("/api/sites", get(sites_handler).post(upsert_site_handler))
        .route("/api/sites/{id}", get(site_summary_handler))
        .route("/api/sites/remove", post(remove_site_handler))
        .route(
            "/api/environments",
            get(environments_handler).post(create_environment_handler),
        )
        .route("/api/environments/update", post(update_environment_handler))
        .route("/api/environments/remove", post(remove_environment_handler))
        .route(
            "/api/environments/assign-site",
            post(assign_site_environment_handler),
        )
        .route("/api/setup/status", get(setup_status_handler))
        .route("/api/profiles", get(profiles_handler))
        .route(
            "/api/profiles/save-custom",
            post(save_custom_profile_handler),
        )
        .route(
            "/api/enrichment",
            get(enrichment_list_handler).post(enrichment_upsert_handler),
        )
        .route("/api/enrichment/remove", post(enrichment_remove_handler))
        .route("/api/enrichment/test", post(enrichment_test_handler))
        .route("/api/enrichment/run", post(enrichment_run_handler))
        .route("/api/enrichment/audit", get(enrichment_audit_handler))
        .route(
            "/api/adapters",
            get(adapter_list_handler).post(adapter_upsert_handler),
        )
        .route("/api/adapters/remove", post(adapter_remove_handler))
        .route("/api/adapters/test", post(adapter_test_handler))
        .route("/api/adapters/audit", get(adapter_audit_handler))
        .route(
            "/api/approvals",
            get(approvals_list_handler).post(approvals_create_handler),
        )
        .route(
            "/api/approvals/{id}/approve",
            post(approvals_approve_handler),
        )
        .route("/api/approvals/{id}/reject", post(approvals_reject_handler))
        .route(
            "/api/approvals/{id}/rollback",
            post(approvals_rollback_handler),
        )
        .route("/api/trust", get(trust_list_handler))
        .route("/api/trust/graduate", post(trust_graduate_handler))
        .route(
            "/api/integrations/servicenow/test",
            post(snow_integration_test_handler),
        )
        .route(
            "/api/integrations/servicenow/aiops/sync",
            post(servicenow_aiops_sync_handler),
        )
        .route(
            "/api/credentials",
            get(credentials_handler).post(add_credential_handler),
        )
        .route("/api/credentials/update", post(update_credential_handler))
        .route("/api/credentials/remove", post(remove_credential_handler))
        .route("/api/credentials/test", post(test_credential_handler))
        .route("/api/detections", get(detections_handler))
        .route("/api/incidents", get(incidents_handler))
        .route("/api/readiness", get(readiness_handler))
        .route("/api/operations", get(operations_handler))
        .route("/api/operations/daily-check", get(daily_check_handler))
        .route("/api/operations/weekly-trend", get(weekly_trend_handler))
        .route("/api/governance/state", get(governance_state_handler))
        .route("/api/_test/status", get(test_status_handler))
        .route(
            "/api/_test/inject_detection",
            post(inject_detection_handler),
        )
        .route(
            "/api/_test/syslog/parse",
            post(parse_syslog_fixture_handler),
        )
        .route("/api/trace/{id}", get(trace_handler))
        .route("/api/events", get(events_handler))
        .route("/api/devices/{address}", get(device_detail_handler))
        .route(
            "/api/devices/{address}/enrichment",
            get(device_enrichment_handler),
        )
        .route("/api/collectors", get(collectors_handler))
        .route(
            "/api/assignment/rules",
            get(assignment_rules_handler).post(set_assignment_rules_handler),
        )
        .route("/api/assignment/status", get(assignment_status_handler))
        .route(
            "/api/assignment/override",
            post(assignment_override_handler),
        )
        // graph insights + explorer (T1-4, T1-5, T1-6)
        .route("/api/graph/insights", get(graph_insights_handler))
        .route("/api/explorer/query", post(explorer_query_handler))
        .route(
            "/api/explorer/saved-queries",
            get(list_saved_queries_handler).post(create_saved_query_handler),
        )
        .route(
            "/api/explorer/saved-queries/{id}/delete",
            post(delete_saved_query_handler),
        )
        // investigations (T3-1/T3-2)
        .route(
            "/api/investigations",
            get(list_investigations_handler).post(create_investigation_handler),
        )
        .route("/api/investigations/{id}", get(get_investigation_handler))
        .route(
            "/api/investigations/{id}/tool-calls",
            get(list_tool_calls_handler),
        )
        .route(
            "/api/investigations/{id}/complete",
            post(complete_investigation_handler),
        )
        // graph embeddings (T2-1)
        .route(
            "/api/graph/embeddings/upsert",
            post(upsert_embeddings_handler),
        )
        .route(
            "/api/graph/embeddings/{address}",
            get(list_embeddings_handler),
        )
        // T5-1: MCP server (agent-friendly JSON-RPC endpoint)
        .route("/mcp", post(crate::mcp_server::mcp_handler))
        // T5-2: Grounded incident response
        .route(
            "/api/incidents/{id}/grounded",
            get(grounded_incident_handler),
        )
        // T5-3: Self-describing OpenAPI schema
        .route("/api/schema", get(schema_handler))
        // T5-5: Natural-language reference resolution
        .route("/api/resolve", get(resolve_handler))
        // CV6 T1-2: Swagger UI + canonical spec endpoint
        .route("/api/docs", get(swagger_ui_handler))
        .route("/api/openapi.json", get(openapi_json_handler))
        // CV7 T4-4: Sidecar registry surface for bonpy UI and ops scripts.
        .route("/api/sidecars", get(sidecars_handler))
        // CV7 T4-6: Liveness gate. Returns "ok" normally; "degraded" with the
        // missing list when BONSAI_REQUIRE_SIDECAR is set and a required kind
        // has not registered within the startup grace window.
        .route("/health", get(health_handler))
        // CV7 T4-5: mount bonpy SPA at /bonpy/. Falls back to index.html for
        // client-side routing inside the bonpy app.
        .nest_service("/bonpy", bonpy_spa)
        .fallback_service(spa)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

pub(crate) mod observability;
pub(crate) mod test_endpoints;
pub(crate) mod discovery;
pub(crate) mod config;
pub(crate) mod governance;
pub(crate) mod schema;
pub(crate) mod mcp_routes;
pub(crate) mod outputs;
pub(crate) mod swagger_ui;
pub(crate) mod openapi_schema;

#[allow(unused_imports)]
pub(crate) use observability::*;
#[allow(unused_imports)]
pub(crate) use test_endpoints::*;
#[allow(unused_imports)]
pub(crate) use discovery::*;
#[allow(unused_imports)]
pub(crate) use config::*;
#[allow(unused_imports)]
pub(crate) use governance::*;
#[allow(unused_imports)]
pub(crate) use schema::*;
#[allow(unused_imports)]
pub(crate) use mcp_routes::*;
#[allow(unused_imports)]
pub(crate) use outputs::*;
#[allow(unused_imports)]
pub(crate) use swagger_ui::*;

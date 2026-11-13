use anyhow::Context;
use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use lbug::{Connection, Value};
use tokio_stream::wrappers::BroadcastStream;
use tonic::{Code, Request, Response, Status, Streaming};

use crate::collector_auth::{
    AuthPolicy, CORE_ADMIN_PROTOCOL, CollectorAuthz, CollectorGrant, DETECTION_INGEST_PROTOCOL,
    OPERATOR_READ_PROTOCOL, ReadScope, SIDECAR_PROTOCOL, STREAM_REVALIDATE_INTERVAL,
};
use crate::config::{SelectedSubscriptionPath, TargetConfig};
use crate::credentials::{CredentialSummary, CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::discovery;
use crate::event_bus::InProcessBus;
use crate::gnmi_set::gnmi_set;
use crate::graph::{GraphStore, SiteRecord};
use crate::ingest;
use crate::registry::{ApiRegistry, DeviceRegistry};
use crate::sidecar_registry::SidecarRegistry;
use crate::store::BonsaiStore;
use crate::telemetry_identity::VerifiedIngressPrincipal;
use crate::write_coordinator::{PriorityWriteRequest, WriteCoordinator};

pub const PROTOCOL_VERSION_CURRENT: u32 = 1;
pub const PROTOCOL_VERSION: u32 = PROTOCOL_VERSION_CURRENT;

/// Version compatibility result. Major skew (>= 10 apart) is a hard error;
/// any other mismatch is a warning; matching versions are compatible.
#[derive(Debug, PartialEq, Eq)]
pub enum VersionCompat {
    Compatible,
    MinorSkew { client: u32, server: u32 },
    MajorSkew { client: u32, server: u32 },
}

pub fn check_protocol_compat(client_version: u32, server_version: u32) -> VersionCompat {
    if client_version == server_version {
        VersionCompat::Compatible
    } else if client_version.abs_diff(server_version) >= 10 || client_version == 0 {
        VersionCompat::MajorSkew {
            client: client_version,
            server: server_version,
        }
    } else {
        VersionCompat::MinorSkew {
            client: client_version,
            server: server_version,
        }
    }
}

pub mod pb {
    #![allow(clippy::all)]
    tonic::include_proto!("bonsai.v1");
}

pub use pb::bonsai_graph_server::{BonsaiGraph, BonsaiGraphServer};
use pb::*;

/// Connection info for a managed target. Credentials stay inside the Rust process.
#[derive(Clone)]
pub struct TargetConnInfo {
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ca_cert_pem: Option<Vec<u8>>,
    pub tls_domain: String,
}

pub struct BonsaiService<S: BonsaiStore> {
    store: Arc<S>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    bus: Arc<InProcessBus>,
    debouncer: Option<Arc<ingest::TelemetryDebouncer>>,
    collector_manager: Option<Arc<crate::assignment::CollectorManager>>,
    sidecar_registry: Arc<SidecarRegistry>,
    sqlite_store: Option<Arc<crate::sqlite_store::SqliteStore>>,
    collector_authz: CollectorAuthz,
    auth_policy: AuthPolicy,
    ha_coordinator: Option<Arc<crate::ha_coordinator::HACoordinator>>,
    detection_coordinator: Option<Arc<WriteCoordinator>>,
}

impl<S: BonsaiStore> BonsaiService<S> {
    pub fn new(
        store: Arc<S>,
        registry: Arc<ApiRegistry>,
        credentials: Arc<CredentialVault>,
        bus: Arc<InProcessBus>,
        debouncer: Option<Arc<ingest::TelemetryDebouncer>>,
        collector_manager: Option<Arc<crate::assignment::CollectorManager>>,
        sidecar_registry: Arc<SidecarRegistry>,
        sqlite_store: Option<Arc<crate::sqlite_store::SqliteStore>>,
        ha_coordinator: Option<Arc<crate::ha_coordinator::HACoordinator>>,
        detection_coordinator: Option<Arc<WriteCoordinator>>,
    ) -> Self {
        let collector_authz = CollectorAuthz::new(sqlite_store.clone());
        Self {
            store,
            registry,
            credentials,
            bus,
            debouncer,
            collector_manager,
            sidecar_registry,
            sqlite_store,
            collector_authz,
            // Fail closed by default: deployments must opt in to
            // development-compat explicitly via runtime.auth_mode.
            auth_policy: AuthPolicy::Required,
            ha_coordinator,
            detection_coordinator,
        }
    }

    /// Set the runtime authentication policy (G5). `server_startup`
    /// derives this from `runtime.auth_mode`; tests may select either.
    pub fn with_auth_policy(mut self, policy: AuthPolicy) -> Self {
        self.auth_policy = policy;
        self
    }

    pub fn with_sqlite_store(mut self, store: Arc<crate::sqlite_store::SqliteStore>) -> Self {
        self.collector_authz = CollectorAuthz::new(Some(Arc::clone(&store)));
        self.sqlite_store = Some(store);
        self
    }

    pub fn with_ha_coordinator(
        mut self,
        ha_coordinator: Arc<crate::ha_coordinator::HACoordinator>,
    ) -> Self {
        self.ha_coordinator = Some(ha_coordinator);
        self
    }

    async fn write_detection_for_ingest(
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
    ) -> Result<String, anyhow::Error> {
        if let Some(coordinator) = &self.detection_coordinator {
            let (reply_to, reply_rx) = tokio::sync::oneshot::channel();
            coordinator
                .submit_priority(PriorityWriteRequest::Detection {
                    device_address,
                    rule_id,
                    severity,
                    features_json,
                    source_types_json,
                    latency_ns,
                    fired_at_ns,
                    state_change_event_id,
                    source_event_ids,
                    reply_to,
                })
                .await?;
            reply_rx.await.context("write coordinator reply dropped")?
        } else {
            self.store
                .write_detection(
                    device_address,
                    rule_id,
                    severity,
                    features_json,
                    source_types_json,
                    latency_ns,
                    fired_at_ns,
                    state_change_event_id,
                    source_event_ids,
                )
                .await
        }
    }

    fn peer_principal<T>(req: &Request<T>) -> Option<VerifiedIngressPrincipal> {
        CollectorAuthz::principal(req)
    }

    /// Resolve the transport principal under the runtime auth policy.
    /// `Required` fails closed on missing/unattested principals;
    /// `DevelopmentCompat` returns `None` for plaintext (legacy).
    fn require_principal<T>(
        &self,
        req: &Request<T>,
    ) -> Result<Option<VerifiedIngressPrincipal>, Status> {
        match Self::peer_principal(req) {
            Some(p) => {
                if !p.is_transport_attested() {
                    return Err(Status::unauthenticated(
                        "peer principal is not transport-attested",
                    ));
                }
                Ok(Some(p))
            }
            None => match self.auth_policy {
                AuthPolicy::Required => Err(Status::unauthenticated(
                    "authenticated transport required (runtime.auth_mode)",
                )),
                AuthPolicy::DevelopmentCompat => Ok(None),
            },
        }
    }

    /// Capability gate for non-ingest RPCs. mTLS peers need an active
    /// credential-pinned binding for `protocol`; dev-compat plaintext
    /// passes; required-mode missing principals already failed.
    fn require_capability<T>(&self, req: &Request<T>, protocol: &str) -> Result<(), Status> {
        if let Some(p) = self.require_principal(req)? {
            self.collector_authz.authorize_capability(&p, protocol)
        } else {
            Ok(())
        }
    }

    /// Any-binding gate for collector core-discovery (`get_leader`).
    /// Deliberately narrow: reveals leadership metadata only.
    fn require_any_binding<T>(&self, req: &Request<T>) -> Result<(), Status> {
        if let Some(p) = self.require_principal(req)? {
            self.collector_authz.authorize_any_binding(&p)
        } else {
            Ok(())
        }
    }

    /// Global operational read gate (`operator-read` capability).
    fn require_global_read<T>(&self, req: &Request<T>) -> Result<(), Status> {
        self.require_capability(req, OPERATOR_READ_PROTOCOL)
    }

    /// Read scope for device-scoped read RPCs (G3). `Global` =
    /// operator-read; `CollectorScoped(ids)` = collector bindings only;
    /// dev-compat plaintext = Global (legacy).
    fn read_scope<T>(&self, req: &Request<T>) -> Result<ReadScope, Status> {
        match self.require_principal(req)? {
            Some(p) => self.collector_authz.read_scope(&p),
            None => Ok(ReadScope::Global),
        }
    }

    /// Whether `address` is readable under `scope`: Global reads
    /// everything; collector-scoped reads only devices assigned to the
    /// credential's collectors.
    fn scope_permits_device(&self, scope: &ReadScope, address: &str) -> bool {
        match scope {
            ReadScope::Global => true,
            ReadScope::CollectorScoped(collector_ids) => {
                let bare = crate::registry::strip_port(address);
                collector_ids.iter().any(|cid| {
                    self.registry
                        .get_device(&bare)
                        .ok()
                        .flatten()
                        .is_some_and(|t| {
                            t.enabled && t.collector_id.as_deref() == Some(cid.as_str())
                        })
                })
            }
        }
    }

    /// Ingest-plane gate evaluated per claimed `collector_id`. Returns
    /// the grant for provenance/audit; dev-compat plaintext yields
    /// `None`.
    fn authorize_ingest(
        &self,
        principal: Option<&VerifiedIngressPrincipal>,
        collector_id: &str,
    ) -> Result<Option<CollectorGrant>, Status> {
        match principal {
            Some(p) => Ok(Some(
                self.collector_authz.authorize_collector(p, collector_id)?,
            )),
            None => Ok(None),
        }
    }

    /// G1+H1 observation-authorization boundary. For an authenticated
    /// collector the DECODED observation is classified with the same
    /// `TelemetryUpdate::classify` the graph writer uses, and the
    /// resulting authority class decides the projection: canonical
    /// classes require an assigned-device target with inventory-rewritten
    /// metadata; provisional classes require a `*src/` source key
    /// namespaced to the authenticated collector. Path prefixes never
    /// establish authority. Dev-compat plaintext keeps legacy behavior.
    fn authorize_observation(
        &self,
        grant: Option<&CollectorGrant>,
        telemetry: &mut crate::telemetry::TelemetryUpdate,
    ) -> Result<(), Status> {
        use crate::telemetry::ObservationAuthority;
        let Some(grant) = grant else {
            return Ok(());
        };
        let event = telemetry.classify();
        match event.authority() {
            ObservationAuthority::Ignored => Ok(()),
            ObservationAuthority::CanonicalDevice => {
                let bare = crate::registry::strip_port(&telemetry.target);
                let target = self.registry.get_device(&bare).map_err(|e| {
                    tracing::error!(%e, "observation authorization registry lookup failed");
                    Status::internal("device registry error")
                })?;
                let authorized = target.as_ref().is_some_and(|t| {
                    t.enabled && t.collector_id.as_deref() == Some(grant.collector_id())
                });
                if !authorized {
                    metrics::counter!(
                        "bonsai_telemetry_observation_denied_total",
                        "reason" => "unauthorized_target"
                    )
                    .increment(1);
                    tracing::warn!(
                        collector_id = %grant.collector_id(),
                        target = %telemetry.target,
                        path = %telemetry.path,
                        "canonical telemetry denied: target not assigned to collector"
                    );
                    return Err(Status::permission_denied(
                        "observation target is not assigned to this collector",
                    ));
                }
                let t = target.expect("checked above");
                // Untrusted metadata never overrides canonical inventory.
                telemetry.vendor = t.vendor.clone().unwrap_or_default();
                telemetry.hostname = t.hostname.clone().unwrap_or_default();
                telemetry.role = t.role.clone().unwrap_or_default();
                telemetry.site = t.site.clone().unwrap_or_default();
                Ok(())
            }
            ObservationAuthority::Provisional => {
                self.authorize_provisional_source(grant, telemetry, &event)
            }
        }
    }

    /// H1 provisional-source boundary. A provisional observation's
    /// `target` IS its graph identity (`StateChangeEvent.device_address`,
    /// AppFlow `exporter_id`), so it must be a recognized `*src/...`
    /// source key — never a canonical device address. The collector
    /// namespace segment is rewritten to the authenticated collector
    /// id AND the protocol prefix is pinned to the classified protocol:
    /// a collector cannot file evidence under another collector's
    /// namespace or under a different protocol's source family. The
    /// original producer claims are preserved under `claimed_*` keys;
    /// the effective identity fields are server-pinned so a missing,
    /// null, numeric or malformed producer claim can never trigger an
    /// unscoped fallback inside `classify()`.
    fn authorize_provisional_source(
        &self,
        grant: &CollectorGrant,
        telemetry: &mut crate::telemetry::TelemetryUpdate,
        event: &crate::telemetry::TelemetryEvent,
    ) -> Result<(), Status> {
        // Fail closed: a provisional authority class without a known
        // protocol prefix is a server-side mapping gap — deny rather
        // than guess a namespace.
        let required_prefix = provisional_prefix_for_event(event).ok_or_else(|| {
            metrics::counter!(
                "bonsai_telemetry_observation_denied_total",
                "reason" => "invalid_provisional_source"
            )
            .increment(1);
            tracing::warn!(
                collector_id = %grant.collector_id(),
                target = %telemetry.target,
                path = %telemetry.path,
                "provisional observation denied: no protocol source prefix"
            );
            Status::permission_denied("provisional observation has no protocol source prefix")
        })?;
        let claimed_target = telemetry.target.clone();
        let normalized = namespace_provisional_source_key(
            &claimed_target,
            grant.collector_id(),
            required_prefix,
        )
        .ok_or_else(|| {
            metrics::counter!(
                "bonsai_telemetry_observation_denied_total",
                "reason" => "invalid_provisional_source"
            )
            .increment(1);
            tracing::warn!(
                collector_id = %grant.collector_id(),
                target = %claimed_target,
                path = %telemetry.path,
                "provisional observation denied: target is not a *src/ source key"
            );
            Status::permission_denied("provisional observation target must be a *src/ source key")
        })?;
        telemetry.target = normalized.clone();
        let Some(map) = telemetry.value.as_object_mut() else {
            // Every provisional protocol carries an object payload.
            // Anything else is malformed — and without an object there
            // is no way to pin the effective identity fields, so a
            // non-object payload must never reach `classify()`'s
            // unscoped fallbacks.
            metrics::counter!(
                "bonsai_telemetry_observation_denied_total",
                "reason" => "invalid_provisional_source"
            )
            .increment(1);
            return Err(Status::invalid_argument(
                "provisional observation payload must be a JSON object",
            ));
        };
        // Preserve the producer's original claim as evidence; the
        // forwarding collector's identity is recorded separately —
        // neither upgrades the observation's authority.
        map.insert(
            "claimed_source_key".to_string(),
            serde_json::Value::String(claimed_target),
        );
        map.insert(
            "forwarding_collector_id".to_string(),
            serde_json::Value::String(grant.collector_id().to_string()),
        );
        // Producer-supplied identity-authority claims are moved into
        // `claimed_*` evidence slots, then the EFFECTIVE fields are
        // pinned server-side. A collector-forwarded observation can
        // never be "resolved" by payload assertion: forwarding
        // collector authentication is not original-source
        // authentication (BN-021g-2 owns verified original-source
        // projection).
        for key in [
            "identity_status",
            "authority",
            "canonical_exporter_id",
            "identity_resolution",
            "device_id",
            "collector_id",
        ] {
            if let Some(original) = map.remove(key) {
                map.insert(format!("claimed_{key}"), original);
            }
        }
        map.insert(
            "identity_status".to_string(),
            serde_json::Value::String("provisional".to_string()),
        );
        map.insert(
            "authority".to_string(),
            serde_json::Value::String("unverified".to_string()),
        );
        for key in ["canonical_exporter_id", "identity_resolution", "device_id"] {
            map.insert(key.to_string(), serde_json::Value::Null);
        }
        // Effective source key for the payload. Every provisional
        // producer sets `source_key`/`exporter_id` to its `*src/` key
        // (syslog/snmp/otlp/sflow producers all emit both fields), and
        // for flow records `exporter_id` is additionally the AppFlow
        // identity — so BOTH fields are pinned unconditionally: a
        // missing, null, numeric or malformed producer claim can never
        // make `classify()` fall back to the untrusted
        // `exporter_address`. A payload source claim that parses as a
        // `*src/` key of the SAME protocol family keeps its own
        // remainder under the authenticated namespace (distinct claimed
        // sources stay distinct); anything else collapses to the
        // normalized target key. Originals are preserved under
        // `claimed_payload_*` evidence keys.
        for key in ["source_key", "exporter_id"] {
            let effective = match map.get(key) {
                Some(serde_json::Value::String(s)) => {
                    namespace_provisional_source_key(s, grant.collector_id(), required_prefix)
                        .unwrap_or_else(|| normalized.clone())
                }
                _ => normalized.clone(),
            };
            if let Some(original) = map.remove(key) {
                map.insert(format!("claimed_payload_{key}"), original);
            }
            map.insert(key.to_string(), serde_json::Value::String(effective));
        }
        // OTLP provisional application ids embed the source key
        // (`otlpapp/{source_key}/{service}`): re-derive them from the
        // normalized key so a forged app id cannot carry a foreign
        // collector namespace. The service-name claim is preserved.
        if let Some(serde_json::Value::String(app_id)) = map.get("provisional_app_id").cloned() {
            let rewritten = app_id
                .strip_prefix("otlpapp/")
                .and_then(|rest| {
                    rest.rfind('/')
                        .map(|i| format!("otlpapp/{normalized}/{}", &rest[i + 1..]))
                })
                .unwrap_or_else(|| format!("otlpapp/{normalized}"));
            if let Some(original) = map.remove("provisional_app_id") {
                map.insert("claimed_provisional_app_id".to_string(), original);
            }
            map.insert(
                "provisional_app_id".to_string(),
                serde_json::Value::String(rewritten),
            );
        }
        Ok(())
    }

    /// Verify remote-detection provenance (G2). The detection target
    /// must be a registry device assigned to the authorized collector,
    /// and every referenced StateChangeEvent must exist, belong to that
    /// same canonical device, and not be provisional-source evidence.
    /// Dev-compat plaintext keeps legacy behavior (returns Ok).
    async fn verify_detection_scope(
        &self,
        grant: Option<&CollectorGrant>,
        device_address: &str,
        event_ids: &[String],
    ) -> Result<(), Status> {
        let Some(grant) = grant else {
            return Ok(());
        };
        if event_ids.len() > crate::collector_auth::MAX_DETECTION_EVENT_REFS {
            return Err(Status::invalid_argument("too many source event references"));
        }
        let bare = crate::registry::strip_port(device_address).to_string();
        let target = self.registry.get_device(&bare).map_err(|e| {
            tracing::error!(%e, "detection authorization registry lookup failed");
            Status::internal("device registry error")
        })?;
        let authorized = target
            .as_ref()
            .is_some_and(|t| t.enabled && t.collector_id.as_deref() == Some(grant.collector_id()));
        if !authorized {
            metrics::counter!(
                "bonsai_detection_ingest_denied_total",
                "reason" => "unauthorized_target"
            )
            .increment(1);
            return Err(Status::permission_denied(
                "detection target is not assigned to this collector",
            ));
        }
        if event_ids.is_empty() {
            // H4: remote DetectionIngest requires positive source-event
            // provenance — an evidence-free remote detection is denied.
            // Evidence-free authoring remains available to core-admin
            // principals via CreateDetection.
            metrics::counter!(
                "bonsai_detection_ingest_denied_total",
                "reason" => "missing_source_events"
            )
            .increment(1);
            return Err(Status::permission_denied(
                "remote detection requires at least one source-event reference",
            ));
        }
        let ids = event_ids.to_vec();
        let want = bare;
        let db = self.store.db();
        let verified = tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db)
                .map_err(|e| ScopeCheckError::retry(format!("{e}")))?;
            for id in &ids {
                let mut stmt = conn
                    .prepare(
                        "MATCH (e:StateChangeEvent {id: $id}) RETURN e.device_address, e.detail, e.source_type",
                    )
                    .map_err(|e| ScopeCheckError::retry(e.to_string()))?;
                let rows = conn
                    .execute(
                        &mut stmt,
                        vec![("id", lbug::Value::String(id.clone()))],
                    )
                    .map_err(|e| ScopeCheckError::retry(e.to_string()))?;
                let row = rows.into_iter().next().ok_or_else(|| {
                    ScopeCheckError::deny(format!(
                        "state change event '{id}' does not exist"
                    ))
                })?;
                let ev_addr = match &row[0] {
                    lbug::Value::String(s) => crate::registry::strip_port(s).to_string(),
                    _ => String::new(),
                };
                if ev_addr != want {
                    return Err(ScopeCheckError::deny(format!(
                        "state change event '{id}' belongs to '{ev_addr}', not '{want}'"
                    )));
                }
                // R2 positive provenance: the event's server-owned
                // `source_type` must be an explicit allowlist of
                // canonical writer types. Missing, null, empty,
                // unknown AND provisional source types all fail
                // closed — a non-empty string is never treated as
                // proof of canonical identity, and `device_address`
                // alone is never treated as proof either (provisional
                // events are keyed by `*src/...` keys that cannot equal
                // a canonical address anyway — this is defense in
                // depth for historical rows).
                let source_type = match &row[2] {
                    lbug::Value::String(s) => s.as_str(),
                    _ => "",
                };
                if !DETECTION_ELIGIBLE_SOURCE_TYPES
                    .iter()
                    .any(|t| source_type.eq_ignore_ascii_case(t))
                {
                    return Err(ScopeCheckError::deny(format!(
                        "state change event '{id}' has no eligible canonical provenance \
                         (source_type '{}')",
                        if source_type.is_empty() { "<missing>" } else { source_type }
                    )));
                }
                // Structured detail check (replaces the serialized-JSON
                // substring match): an event whose detail asserts
                // provisional/unverified identity — at top level or
                // inside `device_context` — is provisional evidence,
                // never detection authority. Unparseable or non-object
                // detail cannot prove non-provisional status and fails
                // closed.
                let detail = match &row[1] {
                    lbug::Value::String(s) => s.as_str(),
                    _ => "",
                };
                let detail_json: serde_json::Value = serde_json::from_str(detail)
                    .map_err(|_| {
                        ScopeCheckError::deny(format!(
                            "state change event '{id}' has malformed event detail"
                        ))
                    })?;
                let detail_obj = detail_json.as_object().ok_or_else(|| {
                    ScopeCheckError::deny(format!(
                        "state change event '{id}' has malformed event detail"
                    ))
                })?;
                let provisional_marker = |v: &serde_json::Value| {
                    v.get("identity_status")
                        .and_then(|s| s.as_str())
                        == Some("provisional")
                        || v.get("authority").and_then(|s| s.as_str()) == Some("unverified")
                };
                let detail_provisional = provisional_marker(&detail_json)
                    || detail_obj
                        .get("device_context")
                        .is_some_and(provisional_marker);
                if detail_provisional {
                    return Err(ScopeCheckError::deny(format!(
                        "state change event '{id}' is provisional/unverified evidence"
                    )));
                }
            }
            Ok::<(), ScopeCheckError>(())
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
        verified.map_err(|err| match err {
            ScopeCheckError::Deny(reason) => {
                metrics::counter!(
                    "bonsai_detection_ingest_denied_total",
                    "reason" => "bad_event_reference"
                )
                .increment(1);
                tracing::warn!(
                    collector_id = %grant.collector_id(),
                    device = %device_address,
                    %reason,
                    "remote detection denied: invalid source-event reference"
                );
                Status::permission_denied(format!("detection rejected: {reason}"))
            }
            ScopeCheckError::Retry(reason) => {
                metrics::counter!(
                    "bonsai_detection_ingest_denied_total",
                    "reason" => "event_lookup_failed"
                )
                .increment(1);
                tracing::error!(
                    collector_id = %grant.collector_id(),
                    device = %device_address,
                    %reason,
                    "remote detection deferred: source-event lookup failed"
                );
                Status::internal(format!("detection source-event lookup failed: {reason}"))
            }
        })
    }

    /// Addresses a collector-scoped read may see; `None` = global.
    fn scoped_addresses(&self, scope: &ReadScope) -> Option<std::collections::HashSet<String>> {
        match scope {
            ReadScope::Global => None,
            ReadScope::CollectorScoped(collector_ids) => {
                let mut addrs = std::collections::HashSet::new();
                for cid in collector_ids {
                    if let Ok(targets) = self.registry.list_assigned_to(cid) {
                        for t in targets {
                            if t.enabled {
                                addrs.insert(crate::registry::strip_port(&t.address).to_string());
                            }
                        }
                    }
                }
                Some(addrs)
            }
        }
    }

    /// Durable provenance record for an authorized ingest stream —
    /// written once per (credential, collector_id) per stream. This is
    /// the durable record distinguishing the authenticated forwarding
    /// collector; per-observation source claims remain provisional
    /// evidence and are never upgraded by this record.
    fn audit_ingest_grant(
        &self,
        principal: &VerifiedIngressPrincipal,
        grant: &CollectorGrant,
        audited: &mut std::collections::HashSet<(String, String)>,
    ) {
        let key = (
            principal.credential_fingerprint().to_string(),
            grant.collector_id().to_string(),
        );
        if !audited.insert(key) {
            return;
        }
        if let Some(store) = &self.sqlite_store {
            let detail = serde_json::json!({
                "binding_id": grant.binding_id(),
                "binding_revision": grant.binding_revision(),
                "scope": grant.scope(),
                "device_id": grant.device_id(),
                "collector_id": grant.collector_id(),
            });
            // G7: audit failure is observable, never silently claimed.
            // The record proves stream-level collector authorization
            // for this (credential, collector, binding revision) — it
            // does NOT authenticate any individual observation's
            // original source. Ingest availability is preserved; the
            // failure is counted and logged for operators.
            if let Err(e) = store.audit(
                "collector_ingest",
                "AUTHZ",
                grant.collector_id(),
                principal.credential_fingerprint(),
                "ingest_authorized",
                None,
                Some(&detail.to_string()),
            ) {
                metrics::counter!("bonsai_audit_write_failure_total", "action" => "collector_ingest")
                    .increment(1);
                tracing::error!(
                    %e,
                    collector_id = %grant.collector_id(),
                    "collector ingest provenance audit write failed"
                );
            }
        }
        metrics::counter!(
            "bonsai_collector_ingest_authorized_total",
            "collector_id" => grant.collector_id().to_string()
        )
        .increment(1);
    }
}

/// Server-established `StateChangeEvent.source_type` values eligible as
/// remote-detection provenance (BN-021g H4/R2). These are exactly the
/// source types the canonical graph writers assign (`gnmi`, `bmp`,
/// `bgp_ls` — matched case-insensitively to cover the historical
/// `gNMI` spelling). Provisional ingestion classes (`syslog`, `snmp`,
/// `netflow`, `sflow`, `otlp`), unknown, empty and missing values are
/// NEVER eligible: provenance is a positive allowlist, not a
/// provisional-denylist.
const DETECTION_ELIGIBLE_SOURCE_TYPES: [&str; 3] = ["gnmi", "bmp", "bgp_ls"];

/// Outcome of the per-event provenance lookup inside
/// [`BonsaiService::verify_detection_scope`]. `Deny` is a permanent
/// provenance rejection (the record can never succeed — it is counted
/// `rejected` and retired); `Retry` is a store/lookup failure (the
/// stream must abort so the batch is replayed, never silently dropped).
enum ScopeCheckError {
    Deny(String),
    Retry(String),
}

impl ScopeCheckError {
    fn deny(reason: impl Into<String>) -> Self {
        Self::Deny(reason.into())
    }
    fn retry(reason: impl Into<String>) -> Self {
        Self::Retry(reason.into())
    }
}

/// H6: RAII guard owning assignment-stream session cleanup. Dropping
/// the guard (normal stream end, client disconnect, or cancellation —
/// including a stream dropped before its first poll) unregisters the
/// collector session under generation fencing. The unregister work is
/// dispatched to the blocking pool so `Drop` never performs blocking
/// I/O on an async worker.
struct AssignmentSessionGuard {
    manager: Arc<crate::assignment::CollectorManager>,
    collector_id: String,
    generation: u64,
}

impl Drop for AssignmentSessionGuard {
    fn drop(&mut self) {
        let manager = Arc::clone(&self.manager);
        let collector_id = self.collector_id.clone();
        let generation = self.generation;
        let unregister = move || {
            manager.unregister_collector(&collector_id, generation);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(unregister);
            }
            Err(_) => unregister(),
        }
    }
}

/// Stream-side revalidation for the assignment channel (G4): the
/// claimed collector id must still be pinned to the connection's
/// credential. Plaintext dev-compat always passes.
fn stream_collector_authorized(
    principal: &Option<VerifiedIngressPrincipal>,
    authz: &CollectorAuthz,
    collector_id: &str,
) -> bool {
    match principal {
        Some(p) => authz.authorize_collector(p, collector_id).is_ok(),
        None => true,
    }
}

/// Provisional protocol evidence source-key prefixes (BN-021d/e/f).
/// Observations keyed by these `*src/...` keys remain provisional
/// downstream — they can never gain canonical device authority through
/// the collector channel.
const PROVISIONAL_SOURCE_PREFIXES: [&str; 5] = [
    "flowsrc/",
    "sflowsrc/",
    "syslogsrc/",
    "snmpsrc/",
    "otlpsrc/",
];

/// Recognized provisional `*src/...` source-key prefix, if any.
fn provisional_source_prefix(key: &str) -> Option<&'static str> {
    PROVISIONAL_SOURCE_PREFIXES
        .iter()
        .copied()
        .find(|p| key.starts_with(p))
}

/// Ingest-stream error classification (H2). Permanent rejections are
/// deterministic client/data faults — replaying the record can never
/// succeed, so it is counted `rejected` and retired with the batch.
/// Everything else (store unavailable, internal errors, timeouts) is
/// retryable: the stream fails and the whole batch is replayed
/// (at-least-once delivery).
fn is_permanent_ingest_rejection(status: &Status) -> bool {
    matches!(
        status.code(),
        Code::InvalidArgument
            | Code::PermissionDenied
            | Code::Unauthenticated
            | Code::FailedPrecondition
            | Code::Unimplemented
            | Code::OutOfRange
    )
}

/// Protocol-correct provisional `*src/` prefix for a classified
/// observation. Every `ObservationAuthority::Provisional` event maps to
/// exactly one prefix; `None` for any other class — callers must fail
/// closed rather than guess a namespace.
fn provisional_prefix_for_event(event: &crate::telemetry::TelemetryEvent) -> Option<&'static str> {
    use crate::telemetry::TelemetryEvent;
    Some(match event {
        TelemetryEvent::NetflowRecord { .. } => "flowsrc/",
        TelemetryEvent::SflowRecord { .. } | TelemetryEvent::SflowCounters { .. } => "sflowsrc/",
        TelemetryEvent::SyslogEvent { .. } | TelemetryEvent::SyslogFact { .. } => "syslogsrc/",
        TelemetryEvent::SnmpTrap { .. } | TelemetryEvent::SnmpFact { .. } => "snmpsrc/",
        TelemetryEvent::OtlpSpan { .. } | TelemetryEvent::OtlpMetrics { .. } => "otlpsrc/",
        _ => return None,
    })
}

/// Rewrite the collector-namespace segment (the path segment directly
/// after the `*src/` prefix) of a provisional source key to the
/// authenticated collector id, preserving the remainder verbatim. The
/// key's protocol prefix MUST equal `required_prefix` — the classified
/// protocol's own `*src/` family — so a producer can never file
/// evidence under a different protocol's source family. The collector
/// id is percent-encoded with the same `encode_key_component` the
/// signal producers use, so ids containing `/`, `%`, whitespace or
/// other delimiters can never produce an ambiguous composite key.
/// Returns `None` when `key` is not a provisional source key of the
/// required protocol family or lacks a collector segment — callers
/// must fail closed.
fn namespace_provisional_source_key(
    key: &str,
    collector_id: &str,
    required_prefix: &str,
) -> Option<String> {
    if provisional_source_prefix(key)? != required_prefix {
        return None;
    }
    let rest = &key[required_prefix.len()..];
    let slash = rest.find('/')?;
    let collector = crate::streaming::netflow::encode_key_component(collector_id);
    Some(format!(
        "{required_prefix}{collector}/{}",
        &rest[slash + 1..]
    ))
}

pub type CollectorService = BonsaiService<crate::collector::graph::CollectorGraphStore>;
pub type CoreService = BonsaiService<GraphStore>;

type EventStream = Pin<Box<dyn Stream<Item = Result<pb::StateEvent, Status>> + Send>>;
type RegisterCollectorStream =
    Pin<Box<dyn Stream<Item = Result<pb::AssignmentUpdate, Status>> + Send>>;

#[tonic::async_trait]
impl<S: BonsaiStore + 'static> BonsaiGraph for BonsaiService<S> {
    type StreamEventsStream = EventStream;
    type RegisterCollectorStream = RegisterCollectorStream;

    // G3 Session 7: GetLeader RPC for collector re-homing
    async fn get_leader(
        &self,
        req: Request<pb::GetLeaderRequest>,
    ) -> Result<Response<pb::GetLeaderResponse>, Status> {
        self.require_any_binding(&req)?;
        let response = if let Some(ref ha_coordinator) = self.ha_coordinator {
            let state = ha_coordinator.state().await;
            let is_leader = matches!(state, crate::ha_coordinator::LeaderState::Leader);
            let leader_id = match state {
                crate::ha_coordinator::LeaderState::Leader => {
                    if let crate::ha_coordinator::HAMode::Cluster { ref node_id } =
                        ha_coordinator.mode
                    {
                        node_id.clone()
                    } else {
                        "standalone".to_string()
                    }
                }
                crate::ha_coordinator::LeaderState::Follower { ref leader_id } => leader_id.clone(),
                crate::ha_coordinator::LeaderState::Electing => "electing".to_string(),
            };

            pb::GetLeaderResponse {
                leader_id,
                leader_addr: std::env::var("BONSAI_GRPC_BIND")
                    .unwrap_or_else(|_| "0.0.0.0:50051".to_string()),
                is_leader,
                error: String::new(),
            }
        } else {
            pb::GetLeaderResponse {
                leader_id: "unknown".to_string(),
                leader_addr: String::new(),
                is_leader: false,
                error: "HA coordinator not configured".to_string(),
            }
        };

        Ok(Response::new(response))
    }

    async fn register_collector(
        &self,
        req: Request<CollectorIdentity>,
    ) -> Result<Response<Self::RegisterCollectorStream>, Status> {
        // G5: peer info; the certificate fingerprint now comes from the
        // same transport-attested principal used for authorization —
        // there is exactly one TLS-identity extraction path (G7).
        let peer_addr = req.remote_addr().map(|addr| addr.to_string());
        // BN-021g: transport-attested principal from the TLS layer.
        // `require_principal` enforces the runtime auth policy (G5).
        let principal = self.require_principal(&req)?;
        let cert_fingerprint = principal
            .as_ref()
            .map(|p| p.credential_fingerprint().to_string());

        let identity = req.into_inner();
        let collector_id = identity.collector_id.clone();
        // BN-021g: when the connection is mTLS-authenticated, the claimed
        // collector_id must be pinned to this exact credential by an
        // active binding — a valid certificate cannot claim another id.
        if let Some(p) = &principal {
            self.collector_authz.authorize_collector(p, &collector_id)?;
        }
        let collector_version = identity.protocol_version;
        let hostname = identity.hostname.clone();
        let auth_token = identity.auth_token;

        // G5: Token validation for collector registration
        let expected_token = std::env::var("BONSAI_COLLECTOR_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());
        if let Some(expected) = expected_token {
            if auth_token != expected {
                tracing::error!(
                    %collector_id,
                    peer_ip = peer_addr.as_deref(),
                    "collector registration rejected: invalid auth token"
                );
                metrics::counter!("bonsai_collector_registration_rejected_total", "reason" => "invalid_token").increment(1);
                if let Some(ref store) = self.sqlite_store {
                    let _ = store.log_collector_registration(
                        &collector_id,
                        &hostname,
                        collector_version,
                        peer_addr.as_deref(),
                        cert_fingerprint.as_deref(),
                        false,
                        Some("invalid_token"),
                    );
                }
                return Err(Status::unauthenticated("invalid collector auth token"));
            }
        }

        // G3 Session 8: Leadership check - only leader should accept collector registrations
        if let Some(ref ha_coordinator) = self.ha_coordinator {
            let is_leader = ha_coordinator.is_leader().await;
            if !is_leader {
                tracing::warn!(
                    %collector_id,
                    peer_ip = peer_addr.as_deref(),
                    "collector registration rejected: this node is not the leader"
                );
                metrics::counter!("bonsai_collector_registration_rejected_total", "reason" => "not_leader").increment(1);
                if let Some(ref store) = self.sqlite_store {
                    let _ = store.log_collector_registration(
                        &collector_id,
                        &hostname,
                        collector_version,
                        peer_addr.as_deref(),
                        cert_fingerprint.as_deref(),
                        false,
                        Some("not_leader"),
                    );
                }
                return Err(Status::unavailable(
                    "this node is not the leader, collectors should connect to the leader",
                ));
            }
        }

        match check_protocol_compat(collector_version, PROTOCOL_VERSION) {
            VersionCompat::Compatible => {
                tracing::info!(
                    %collector_id,
                    collector_version,
                    server_version = PROTOCOL_VERSION,
                    "collector registered"
                );
            }
            VersionCompat::MinorSkew { client, server } => {
                tracing::warn!(
                    %collector_id,
                    collector_version = client,
                    server_version = server,
                    "collector protocol version mismatch — minor skew, continuing"
                );
            }
            VersionCompat::MajorSkew { client, server } => {
                tracing::error!(
                    %collector_id,
                    peer_ip = peer_addr.as_deref(),
                    collector_version = client,
                    server_version = server,
                    "collector protocol version major skew — rejecting registration"
                );
                metrics::counter!("bonsai_collector_registration_rejected_total", "reason" => "protocol_skew").increment(1);
                if let Some(ref store) = self.sqlite_store {
                    let _ = store.log_collector_registration(
                        &collector_id,
                        &hostname,
                        collector_version,
                        peer_addr.as_deref(),
                        cert_fingerprint.as_deref(),
                        false,
                        Some(&format!(
                            "protocol_skew: collector={client} server={server}"
                        )),
                    );
                }
                return Err(Status::failed_precondition(format!(
                    "protocol version major skew: collector={client} server={server}; upgrade required"
                )));
            }
        }

        // Log successful registration
        if let Some(ref store) = self.sqlite_store {
            let _ = store.log_collector_registration(
                &collector_id,
                &hostname,
                collector_version,
                peer_addr.as_deref(),
                cert_fingerprint.as_deref(),
                true,
                None,
            );
        }

        let manager = self
            .collector_manager
            .as_ref()
            .ok_or_else(|| Status::unimplemented("collector manager not enabled on this node"))?;

        let (mut rx, generation) = manager
            .register_collector(collector_id.clone())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let collector_id_for_stream = collector_id.clone();
        let principal_for_stream = principal.clone();
        let authz_for_stream = self.collector_authz.clone();
        // H6: session cleanup is owned by a drop guard captured by the
        // stream — a client disconnect or cancellation drops the
        // generator (polled or not) and the guard still unregisters the
        // session under generation fencing.
        let session_guard = AssignmentSessionGuard {
            manager: Arc::clone(manager),
            collector_id: collector_id.clone(),
            generation,
        };

        let stream = async_stream::stream! {
            let _session_guard = session_guard;
            // G4: bounded revalidation of this credential-bearing stream.
            // Authorization is re-checked before the initial assignment
            // delivery, before every subsequent delivery, and
            // periodically while idle — a revoked principal loses future
            // stream access within ~AUTHZ_CACHE_TTL. Session generation
            // fencing ensures a stale close cannot unregister a newer
            // session for the same collector id.
            let mut revalidate = tokio::time::interval(STREAM_REVALIDATE_INTERVAL);
            revalidate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Some(update) => {
                            if !stream_collector_authorized(
                                &principal_for_stream,
                                &authz_for_stream,
                                &collector_id_for_stream,
                            ) {
                                break;
                            }
                            yield Ok(update);
                        }
                        None => break,
                    },
                    _ = revalidate.tick() => {
                        if !stream_collector_authorized(
                            &principal_for_stream,
                            &authz_for_stream,
                            &collector_id_for_stream,
                        ) {
                            break;
                        }
                    }
                }
            }
            // `_session_guard` drops here (and on cancellation),
            // unregistering the session under generation fencing.
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn heartbeat(
        &self,
        req: Request<pb::CollectorStats>,
    ) -> Result<Response<pb::HeartbeatAck>, Status> {
        let principal = self.require_principal(&req)?;
        let stats = req.into_inner();
        if let Some(p) = &principal {
            self.collector_authz
                .authorize_collector(p, &stats.collector_id)?;
        }
        if let Some(manager) = &self.collector_manager {
            let receiver_records: Vec<crate::assignment::ReceiverStatusRecord> = stats
                .receiver_statuses
                .iter()
                .map(|r| crate::assignment::ReceiverStatusRecord {
                    name: r.name.clone(),
                    state: r.state.clone(),
                    addr: r.addr.clone(),
                    packet_count: r.packet_count,
                    error_count: r.error_count,
                    last_error: if r.last_error.is_empty() {
                        None
                    } else {
                        Some(r.last_error.clone())
                    },
                })
                .collect();
            manager.record_heartbeat(
                &stats.collector_id,
                stats.queue_depth_updates,
                stats.subscription_count,
                stats.uptime_secs,
                stats.queue_bytes,
                stats.queue_utilization_pct,
                stats.active_subscribers,
                stats.failed_subscribers,
                stats.memory_used_bytes,
                stats.recent_warn_count,
                stats.recent_error_count,
                receiver_records,
            );
        }
        tracing::debug!(
            collector_id = %stats.collector_id,
            queue_depth = stats.queue_depth_updates,
            subs = stats.subscription_count,
            uptime = stats.uptime_secs,
            "collector heartbeat received"
        );
        Ok(Response::new(pb::HeartbeatAck {
            error: String::new(),
        }))
    }

    async fn query(&self, req: Request<QueryRequest>) -> Result<Response<QueryResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let cypher = req.into_inner().cypher;
        let db = self.store.db();

        let result = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let rows = conn.query(&cypher).map_err(|e| e.to_string())?;
            let mut out: Vec<Vec<serde_json::Value>> = Vec::new();
            for row in rows {
                out.push(row.iter().map(value_to_json).collect());
            }
            serde_json::to_string(&out).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        match result {
            Ok(json_rows) => Ok(Response::new(QueryResponse {
                json_rows,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(QueryResponse {
                json_rows: String::new(),
                error: e,
            })),
        }
    }

    async fn get_devices(
        &self,
        req: Request<GetDevicesRequest>,
    ) -> Result<Response<GetDevicesResponse>, Status> {
        // G3: global inventory requires operator-read; collector
        // credentials get only their assigned devices.
        let scope = self.read_scope(&req)?;
        let allowed = self.scoped_addresses(&scope);
        let db = self.store.db();
        let devices = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let rows = conn
                .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname, d.updated_at")
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                let address = str_val(&row[0]);
                if let Some(ref allowed) = allowed
                    && !allowed.contains(crate::registry::strip_port(&address))
                {
                    continue;
                }
                out.push(Device {
                    address,
                    vendor: str_val(&row[1]),
                    hostname: str_val(&row[2]),
                    updated_at_ns: ts_val(&row[3]),
                });
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetDevicesResponse { devices }))
    }

    async fn list_managed_devices(
        &self,
        req: Request<pb::ListManagedDevicesRequest>,
    ) -> Result<Response<pb::ListManagedDevicesResponse>, Status> {
        // G3: collector credentials see only devices assigned to their
        // collectors; global listing requires operator-read.
        let scope = self.read_scope(&req)?;
        let devices = match &scope {
            ReadScope::Global => self
                .registry
                .list_active()
                .map_err(|e| Status::internal(e.to_string()))?,
            ReadScope::CollectorScoped(collector_ids) => {
                let mut out = Vec::new();
                for cid in collector_ids {
                    if let Ok(targets) = self.registry.list_assigned_to(cid) {
                        out.extend(targets);
                    }
                }
                out
            }
        }
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| managed_device_from_target(&t))
        .collect();

        Ok(Response::new(pb::ListManagedDevicesResponse { devices }))
    }

    async fn add_device(
        &self,
        req: Request<pb::AddDeviceRequest>,
    ) -> Result<Response<pb::DeviceMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self
            .registry
            .add_device_with_audit(target, "grpc", "grpc_add_device")
        {
            Ok(target) => {
                if let Err(error) = self
                    .store
                    .sync_sites_from_targets(vec![target.clone()])
                    .await
                {
                    return Ok(Response::new(pb::DeviceMutationResponse {
                        success: false,
                        error: format!("device saved but site graph sync failed: {error:#}"),
                        device: Some(managed_device_from_target(&target)),
                    }));
                }
                Ok(Response::new(pb::DeviceMutationResponse {
                    success: true,
                    error: String::new(),
                    device: Some(managed_device_from_target(&target)),
                }))
            }
            Err(e) => Ok(Response::new(pb::DeviceMutationResponse {
                success: false,
                error: e.to_string(),
                device: None,
            })),
        }
    }

    async fn update_device(
        &self,
        req: Request<pb::UpdateDeviceRequest>,
    ) -> Result<Response<pb::DeviceMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self
            .registry
            .update_device_with_audit(target, "grpc", "grpc_update_device")
        {
            Ok(target) => {
                if let Err(error) = self
                    .store
                    .sync_sites_from_targets(vec![target.clone()])
                    .await
                {
                    return Ok(Response::new(pb::DeviceMutationResponse {
                        success: false,
                        error: format!("device saved but site graph sync failed: {error:#}"),
                        device: Some(managed_device_from_target(&target)),
                    }));
                }
                Ok(Response::new(pb::DeviceMutationResponse {
                    success: true,
                    error: String::new(),
                    device: Some(managed_device_from_target(&target)),
                }))
            }
            Err(e) => Ok(Response::new(pb::DeviceMutationResponse {
                success: false,
                error: e.to_string(),
                device: None,
            })),
        }
    }

    async fn remove_device(
        &self,
        req: Request<RemoveDeviceRequest>,
    ) -> Result<Response<DeviceMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let address = req.into_inner().address;
        match self.registry.remove_device(&address) {
            Ok(Some(target)) => {
                // If the device was assigned to a collector, we should ideally notify it.
                // For now, we rely on full sync or future explicit 'Remove' command.
                Ok(Response::new(DeviceMutationResponse {
                    success: true,
                    error: String::new(),
                    device: Some(managed_device_from_target(&target)),
                }))
            }
            Ok(None) => Ok(Response::new(DeviceMutationResponse {
                success: false,
                error: format!("device '{address}' not found"),
                device: None,
            })),
            Err(e) => Ok(Response::new(DeviceMutationResponse {
                success: false,
                error: e.to_string(),
                device: None,
            })),
        }
    }

    async fn discover_device(
        &self,
        req: Request<pb::DiscoverRequest>,
    ) -> Result<Response<pb::DiscoveryReport>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let request = req.into_inner();
        let credentials = resolve_request_credentials(
            &self.credentials,
            option_string(request.credential_alias),
            option_string(request.username_env),
            option_string(request.password_env),
        )
        .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
        let (username, password) = match credentials {
            Some(credentials) => {
                let password = credentials.password_string();
                (Some(credentials.username), Some(password))
            }
            None => (None, None),
        };
        let input = discovery::DiscoveryInput {
            address: request.address,
            username,
            password,
            username_env: None,
            password_env: None,
            ca_cert_path: option_string(request.ca_cert_path),
            tls_domain: option_string(request.tls_domain),
            role_hint: option_string(request.role_hint),
            environment_archetype: None,
            vault: None,
        };

        let report = discovery::discover_device(input)
            .await
            .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;

        Ok(Response::new(discovery_report_to_proto(report)))
    }

    async fn list_sites(
        &self,
        req: Request<pb::ListSitesRequest>,
    ) -> Result<Response<pb::ListSitesResponse>, Status> {
        // G3: site hierarchy is global topology — operator-read only.
        self.require_global_read(&req)?;
        let sites = self
            .store
            .list_sites()
            .await
            .map_err(|e| Status::internal(format!("{e:#}")))?
            .into_iter()
            .map(site_to_proto)
            .collect();
        Ok(Response::new(pb::ListSitesResponse { sites }))
    }

    async fn add_site(
        &self,
        req: Request<pb::AddSiteRequest>,
    ) -> Result<Response<pb::SiteMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let site = site_from_proto(req.into_inner().site)
            .map_err(|e| Status::invalid_argument(format!("{e:#}")))?;
        match self.store.upsert_site(site).await {
            Ok(site) => Ok(Response::new(pb::SiteMutationResponse {
                success: true,
                error: String::new(),
                site: Some(site_to_proto(site)),
            })),
            Err(error) => Ok(Response::new(pb::SiteMutationResponse {
                success: false,
                error: format!("{error:#}"),
                site: None,
            })),
        }
    }

    async fn update_site(
        &self,
        req: Request<pb::UpdateSiteRequest>,
    ) -> Result<Response<pb::SiteMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let site = site_from_proto(req.into_inner().site)
            .map_err(|e| Status::invalid_argument(format!("{e:#}")))?;
        match self.store.upsert_site(site).await {
            Ok(site) => Ok(Response::new(pb::SiteMutationResponse {
                success: true,
                error: String::new(),
                site: Some(site_to_proto(site)),
            })),
            Err(error) => Ok(Response::new(pb::SiteMutationResponse {
                success: false,
                error: format!("{error:#}"),
                site: None,
            })),
        }
    }

    async fn list_credentials(
        &self,
        req: Request<pb::ListCredentialsRequest>,
    ) -> Result<Response<pb::ListCredentialsResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let credentials = self
            .credentials
            .list()
            .map_err(|e| Status::failed_precondition(format!("{e:#}")))?
            .into_iter()
            .map(credential_to_proto)
            .collect();
        Ok(Response::new(pb::ListCredentialsResponse { credentials }))
    }

    async fn add_credential(
        &self,
        req: Request<pb::AddCredentialRequest>,
    ) -> Result<Response<pb::CredentialMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let req = req.into_inner();
        match self
            .credentials
            .add(&req.alias, &req.username, &req.password)
        {
            Ok(credential) => Ok(Response::new(pb::CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_to_proto(credential)),
            })),
            Err(error) => Ok(Response::new(pb::CredentialMutationResponse {
                success: false,
                error: format!("{error:#}"),
                credential: None,
            })),
        }
    }

    async fn remove_credential(
        &self,
        req: Request<pb::RemoveCredentialRequest>,
    ) -> Result<Response<pb::CredentialMutationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let alias = req.into_inner().alias;
        match self.credentials.remove(&alias) {
            Ok(Some(credential)) => Ok(Response::new(pb::CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_to_proto(credential)),
            })),
            Ok(None) => Ok(Response::new(pb::CredentialMutationResponse {
                success: false,
                error: format!("credential alias '{alias}' not found"),
                credential: None,
            })),
            Err(error) => Ok(Response::new(pb::CredentialMutationResponse {
                success: false,
                error: format!("{error:#}"),
                credential: None,
            })),
        }
    }

    async fn get_interfaces(
        &self,
        req: Request<GetInterfacesRequest>,
    ) -> Result<Response<GetInterfacesResponse>, Status> {
        // G3: empty device_address = global read (operator-read);
        // a specific device is allowed under collector scope when the
        // device is assigned to the credential's collector.
        let scope = self.read_scope(&req)?;
        let device_address = req.into_inner().device_address;
        if device_address.is_empty() {
            if !matches!(scope, ReadScope::Global) {
                return Err(Status::permission_denied(
                    "global interface read requires operator-read capability",
                ));
            }
        } else if !self.scope_permits_device(&scope, &device_address) {
            return Err(Status::permission_denied(
                "device is outside this credential's read scope",
            ));
        }
        let db = self.store.db();

        let interfaces = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut out = Vec::new();

            if device_address.is_empty() {
                let rows = conn
                    .query(
                        "MATCH (i:Interface) RETURN i.device_address, i.name, \
                     i.in_pkts, i.out_pkts, i.in_octets, i.out_octets, \
                     i.in_errors, i.out_errors, i.updated_at",
                    )
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    out.push(interface_from_row(&row));
                }
            } else {
                let mut stmt = conn.prepare(
                    "MATCH (i:Interface {device_address: $addr}) RETURN i.device_address, i.name, \
                     i.in_pkts, i.out_pkts, i.in_octets, i.out_octets, \
                     i.in_errors, i.out_errors, i.updated_at",
                ).map_err(|e| e.to_string())?;
                let rows = conn
                    .execute(&mut stmt, vec![("addr", Value::String(device_address))])
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    out.push(interface_from_row(&row));
                }
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetInterfacesResponse { interfaces }))
    }

    async fn get_bgp_neighbors(
        &self,
        req: Request<GetBgpNeighborsRequest>,
    ) -> Result<Response<GetBgpNeighborsResponse>, Status> {
        // G3: same scoping rule as get_interfaces.
        let scope = self.read_scope(&req)?;
        let device_address = req.into_inner().device_address;
        if device_address.is_empty() {
            if !matches!(scope, ReadScope::Global) {
                return Err(Status::permission_denied(
                    "global BGP neighbor read requires operator-read capability",
                ));
            }
        } else if !self.scope_permits_device(&scope, &device_address) {
            return Err(Status::permission_denied(
                "device is outside this credential's read scope",
            ));
        }
        let db = self.store.db();

        let neighbors = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut out = Vec::new();

            if device_address.is_empty() {
                let rows = conn.query(
                    "MATCH (n:BgpNeighbor) RETURN n.device_address, n.peer_address, \
                     n.peer_as, n.session_state, n.updated_at",
                ).map_err(|e| e.to_string())?;
                for row in rows {
                    out.push(bgp_neighbor_from_row(&row));
                }
            } else {
                let mut stmt = conn.prepare(
                    "MATCH (n:BgpNeighbor {device_address: $addr}) RETURN n.device_address, n.peer_address, \
                     n.peer_as, n.session_state, n.updated_at",
                ).map_err(|e| e.to_string())?;
                let rows = conn.execute(&mut stmt, vec![("addr", Value::String(device_address))])
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    out.push(bgp_neighbor_from_row(&row));
                }
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetBgpNeighborsResponse { neighbors }))
    }

    async fn get_topology(
        &self,
        req: Request<GetTopologyRequest>,
    ) -> Result<Response<GetTopologyResponse>, Status> {
        // G3: topology is global — operator-read only. Collector
        // credentials are denied rather than given a pretend filter.
        self.require_global_read(&req)?;
        let db = self.store.db();
        let edges = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let rows = conn
                .query(
                    "MATCH (li:Interface)-[:CONNECTED_TO]->(ri:Interface) \
                 RETURN li.device_address, li.name, ri.device_address, ri.name",
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(TopologyEdge {
                    src_device: str_val(&row[0]),
                    src_interface: str_val(&row[1]),
                    dst_device: str_val(&row[2]),
                    dst_interface: str_val(&row[3]),
                });
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetTopologyResponse { edges }))
    }

    async fn stream_events(
        &self,
        req: Request<StreamEventsRequest>,
    ) -> Result<Response<EventStream>, Status> {
        // G3+G4: the event stream exposes global state — requires the
        // operator-read capability, and the authorization is
        // revalidated periodically for the life of the stream so a
        // revoked principal loses access within ~AUTHZ_CACHE_TTL even
        // when the stream is idle.
        let principal = self.require_principal(&req)?;
        if let Some(p) = &principal {
            self.collector_authz
                .authorize_capability(p, OPERATOR_READ_PROTOCOL)?;
        }
        let inner = req.into_inner();
        let filter_types = inner.event_types;
        let filter_device = inner.device_address;
        let rx = self.store.subscribe_events();
        let authz_for_stream = self.collector_authz.clone();

        let stream = async_stream::stream! {
            let events = BroadcastStream::new(rx).filter_map({
                let filter_types = filter_types.clone();
                let filter_device = filter_device.clone();
                move |item| {
                    let filter_types = filter_types.clone();
                    let filter_device = filter_device.clone();
                    async move {
                        match item {
                            Ok(ev) => {
                                if !filter_device.is_empty() && ev.device_address != filter_device {
                                    return None;
                                }
                                if !filter_types.is_empty() && !filter_types.contains(&ev.event_type) {
                                    return None;
                                }
                                Some(Ok(pb::StateEvent {
                                    device_address: ev.device_address,
                                    event_type: ev.event_type,
                                    detail_json: ev.detail_json,
                                    occurred_at_ns: ev.occurred_at_ns,
                                    state_change_event_id: ev.state_change_event_id,
                                }))
                            }
                            Err(_) => Some(Err(Status::internal("broadcast stream error"))),
                        }
                    }
                }
            });
            futures::pin_mut!(events);
            let mut revalidate = tokio::time::interval(STREAM_REVALIDATE_INTERVAL);
            revalidate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    item = events.next() => match item {
                        Some(ev) => {
                            let still_ok = match &principal {
                                Some(p) => authz_for_stream
                                    .authorize_capability(p, OPERATOR_READ_PROTOCOL)
                                    .is_ok(),
                                None => true,
                            };
                            if !still_ok {
                                break;
                            }
                            yield ev;
                        }
                        None => break,
                    },
                    _ = revalidate.tick() => {
                        let still_ok = match &principal {
                            Some(p) => authz_for_stream
                                .authorize_capability(p, OPERATOR_READ_PROTOCOL)
                                .is_ok(),
                            None => true,
                        };
                        if !still_ok {
                            break;
                        }
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn telemetry_ingest(
        &self,
        req: Request<Streaming<pb::TelemetryIngestUpdate>>,
    ) -> Result<Response<pb::TelemetryIngestResponse>, Status> {
        // BN-021g: the peer principal is per-CONNECTION (TLS layer);
        // each message's claimed collector_id is authorized separately
        // against credential-pinned bindings. Cached decisions bound the
        // revocation window to AUTHZ_CACHE_TTL (5s).
        let principal = self.require_principal(&req)?;
        let mut audited = std::collections::HashSet::new();
        let mut stream = req.into_inner();
        let mut accepted = 0_u64;
        let mut rejected = 0_u64;

        while let Some(update) = stream
            .message()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            let collector_id = update.collector_id.clone();
            // R3: protocol version is verified on EVERY record, not
            // just the first — a major-skew record arriving mid-stream
            // must never bypass verification. Major skew is a
            // permanent per-record rejection; minor skew is logged.
            match check_protocol_compat(update.protocol_version, PROTOCOL_VERSION_CURRENT) {
                VersionCompat::Compatible => {}
                VersionCompat::MinorSkew { client, server } => {
                    metrics::counter!(
                        "bonsai_telemetry_ingest_version_skew_total",
                        "skew" => "minor"
                    )
                    .increment(1);
                    tracing::debug!(
                        collector_id = %update.collector_id,
                        collector_version = client,
                        server_version = server,
                        "telemetry record protocol version minor skew"
                    );
                }
                VersionCompat::MajorSkew { client, server } => {
                    // Permanent per-record rejection: a skewed
                    // collector's records are retired with the batch
                    // rather than replayed forever.
                    tracing::warn!(
                        collector_id = %update.collector_id,
                        collector_version = client,
                        server_version = server,
                        "telemetry record rejected: protocol version major skew"
                    );
                    metrics::counter!(
                        "bonsai_telemetry_ingest_rejected_total",
                        "reason" => "protocol_skew"
                    )
                    .increment(1);
                    rejected += 1;
                    continue;
                }
            }

            // Per-record collector authorization. Permanent denials
            // (unbound/ambiguous/revoked credential bindings) retire the
            // record; retryable store failures abort the stream so the
            // batch is replayed (at-least-once).
            let mut grant = None;
            if let Some(p) = &principal {
                match self.authorize_ingest(Some(p), &collector_id) {
                    Ok(Some(g)) => {
                        self.audit_ingest_grant(p, &g, &mut audited);
                        grant = Some(g);
                    }
                    Ok(None) => {}
                    Err(status) if is_permanent_ingest_rejection(&status) => {
                        rejected += 1;
                        continue;
                    }
                    Err(status) => return Err(status),
                }
            }

            // Decode the bounded observation BEFORE authorization so the
            // authority class decided here is the same one the graph
            // writer will project. Malformed payloads are permanent
            // rejections — a poison record must not retry forever.
            let mut telemetry = match ingest::ingest_update_to_telemetry(update) {
                Ok(t) => t,
                Err(e) => {
                    rejected += 1;
                    metrics::counter!(
                        "bonsai_telemetry_ingest_rejected_total",
                        "reason" => "undecodable"
                    )
                    .increment(1);
                    tracing::warn!(%collector_id, %e, "telemetry record rejected: undecodable payload");
                    continue;
                }
            };

            // G1+H1: observation authorization — an authenticated
            // collector may only project canonical telemetry for
            // devices assigned to it (metadata rewritten from
            // inventory), and provisional evidence only under its own
            // `*src/` namespace. Denied observations are dropped,
            // never published.
            if let Err(status) = self.authorize_observation(grant.as_ref(), &mut telemetry) {
                if is_permanent_ingest_rejection(&status) {
                    rejected += 1;
                    continue;
                }
                return Err(status);
            }

            if let Some(ref d) = self.debouncer
                && d.should_drop(&telemetry)
            {
                accepted += 1;
                continue;
            }

            // R3 bus-delivery semantics: `accepted` means the
            // observation was ENQUEUED to the event bus router —
            // delivered to subscribers is a separate, explicitly
            // configured matter (subscriber overflow policies and the
            // broadcast tap are documented lossy surfaces). A router
            // overload is a TRANSIENT failure, not a permanent
            // rejection: abort the stream without a response so the
            // collector retains and replays the whole batch
            // (at-least-once) instead of acknowledging a silent drop.
            match self.bus.publish(telemetry) {
                crate::event_bus::PublishOutcome::Enqueued => {
                    accepted += 1;
                }
                crate::event_bus::PublishOutcome::Overloaded => {
                    metrics::counter!(
                        "bonsai_telemetry_ingest_rejected_total",
                        "reason" => "bus_overload"
                    )
                    .increment(1);
                    tracing::warn!(
                        %collector_id,
                        accepted,
                        "telemetry ingest aborted: event bus router queue full"
                    );
                    return Err(Status::unavailable(
                        "event bus router queue full; batch retained for replay",
                    ));
                }
            }

            if accepted == 1 || accepted.is_multiple_of(1_000) {
                tracing::debug!(%collector_id, accepted, "accepted telemetry ingest updates");
            }
        }

        if rejected > 0 {
            tracing::warn!(
                accepted,
                rejected,
                "telemetry ingest rejected unauthorized/invalid records"
            );
        }
        Ok(Response::new(pb::TelemetryIngestResponse {
            accepted,
            error: String::new(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
            rejected,
        }))
    }

    async fn detection_ingest(
        &self,
        req: Request<Streaming<pb::DetectionEventIngest>>,
    ) -> Result<Response<pb::DetectionIngestResponse>, Status> {
        // G5+G2: transport policy first, then the SEPARATE detection
        // capability — ordinary collector ingest authority never implies
        // detection-authoring authority.
        let principal = self.require_principal(&req)?;
        if let Some(p) = &principal {
            self.collector_authz
                .authorize_capability(p, DETECTION_INGEST_PROTOCOL)?;
        }
        let mut audited = std::collections::HashSet::new();
        let mut stream = req.into_inner();
        let mut accepted = 0_u64;
        let mut rejected = 0_u64;

        while let Some(d) = stream
            .message()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            let collector_id = d.collector_id.clone();

            // H3: revalidate the SEPARATE detection-ingest capability for
            // EVERY message, not just at stream start. A capability
            // revoked mid-stream takes effect on the next message
            // (bounded by the authz decision cache/epoch), and ordinary
            // collector ingest authority never substitutes for it.
            // Capability loss ends the stream — every subsequent message
            // would fail identically.
            if let Some(p) = &principal
                && let Err(status) = self
                    .collector_authz
                    .authorize_capability(p, DETECTION_INGEST_PROTOCOL)
            {
                return Err(status);
            }

            // Per-record collector authorization — same permanent vs
            // retryable split as telemetry ingest.
            let mut grant = None;
            if let Some(p) = &principal {
                match self.authorize_ingest(Some(p), &collector_id) {
                    Ok(Some(g)) => {
                        self.audit_ingest_grant(p, &g, &mut audited);
                        grant = Some(g);
                    }
                    Ok(None) => {}
                    Err(status) if is_permanent_ingest_rejection(&status) => {
                        rejected += 1;
                        continue;
                    }
                    Err(status) => return Err(status),
                }
            }

            // H4: authenticated remote detections must carry parseable
            // payloads and positive source-event provenance. Malformed
            // JSON is a permanent rejection.
            if grant.is_some() {
                if serde_json::from_str::<serde_json::Value>(&d.features_json).is_err()
                    || serde_json::from_str::<Vec<String>>(&d.source_types_json).is_err()
                {
                    rejected += 1;
                    metrics::counter!(
                        "bonsai_detection_ingest_denied_total",
                        "reason" => "malformed_payload"
                    )
                    .increment(1);
                    continue;
                }
            }

            // G2+H4: verify target scope and source-event provenance
            // before any authoritative DetectionEvent write. Failures
            // drop the detection — they never reach the graph.
            let mut event_ids = d.source_event_ids.clone();
            if !d.state_change_event_id.is_empty() {
                event_ids.push(d.state_change_event_id.clone());
            }
            if let Err(status) = self
                .verify_detection_scope(grant.as_ref(), &d.device_address, &event_ids)
                .await
            {
                if is_permanent_ingest_rejection(&status) {
                    rejected += 1;
                    tracing::warn!(
                        %collector_id,
                        device = %d.device_address,
                        code = ?status.code(),
                        "remote detection dropped by authorization boundary"
                    );
                    continue;
                }
                // Retryable store/lookup failure: abort the stream so
                // the whole batch is replayed rather than silently
                // dropping a detection that may be valid.
                return Err(status);
            }

            // H4 provenance: mark detections submitted through the remote
            // ingest channel so stored `source_types` distinguishes them
            // from locally-authored (core-admin) detections.
            let source_types_json = if grant.is_some() {
                let mut types: Vec<String> =
                    serde_json::from_str(&d.source_types_json).unwrap_or_default();
                types.insert(0, "remote_detection_ingest".to_string());
                serde_json::to_string(&types).unwrap_or_else(|_| d.source_types_json.clone())
            } else {
                d.source_types_json.clone()
            };

            match self
                .write_detection_for_ingest(
                    d.device_address,
                    d.rule_id,
                    d.severity,
                    d.features_json,
                    source_types_json,
                    d.latency_ns,
                    d.fired_at_ns,
                    d.state_change_event_id,
                    d.source_event_ids,
                )
                .await
            {
                Ok(_) => {
                    accepted += 1;
                }
                Err(error) => {
                    // Storage failure is retryable, not a permanent
                    // rejection — abort the stream so the caller knows
                    // this detection was not durably written.
                    return Err(Status::unavailable(format!(
                        "detection write failed: {error}"
                    )));
                }
            }
        }

        if rejected > 0 {
            tracing::warn!(
                accepted,
                rejected,
                "detection ingest rejected unauthorized/invalid detections"
            );
        }
        Ok(Response::new(pb::DetectionIngestResponse {
            accepted,
            error: String::new(),
            rejected,
        }))
    }

    async fn create_detection(
        &self,
        req: Request<CreateDetectionRequest>,
    ) -> Result<Response<CreateDetectionResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let r = req.into_inner();
        match self
            .write_detection_for_ingest(
                r.device_address,
                r.rule_id,
                r.severity,
                r.features_json,
                r.source_types_json,
                r.latency_ns,
                r.fired_at_ns,
                r.state_change_event_id,
                r.source_event_ids,
            )
            .await
        {
            Ok(id) => Ok(Response::new(CreateDetectionResponse {
                id,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(CreateDetectionResponse {
                id: String::new(),
                error: format!("{:#}", e),
            })),
        }
    }

    async fn create_remediation(
        &self,
        req: Request<CreateRemediationRequest>,
    ) -> Result<Response<CreateRemediationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let r = req.into_inner();
        match self
            .store
            .write_remediation(
                r.detection_id,
                r.action,
                r.status,
                r.detail_json,
                r.attempted_at_ns,
                r.completed_at_ns,
            )
            .await
        {
            Ok(id) => Ok(Response::new(CreateRemediationResponse {
                id,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(CreateRemediationResponse {
                id: String::new(),
                error: format!("{:#}", e),
            })),
        }
    }

    async fn push_remediation(
        &self,
        req: Request<PushRemediationRequest>,
    ) -> Result<Response<PushRemediationResponse>, Status> {
        self.require_capability(&req, CORE_ADMIN_PROTOCOL)?;
        let r = req.into_inner();
        let target = self
            .registry
            .get_device(&r.target_address)
            .map_err(|e| Status::internal(e.to_string()))?;
        let Some(target) = target else {
            return Ok(Response::new(PushRemediationResponse {
                success: false,
                error: format!("unknown target '{}'", r.target_address),
            }));
        };

        let conn_info = target_conn_info_from_config(&target, &self.credentials).await?;
        let result = gnmi_set(
            &conn_info.address,
            conn_info.username.as_deref(),
            conn_info.password.as_deref(),
            conn_info.ca_cert_pem.as_deref(),
            &conn_info.tls_domain,
            &r.yang_path,
            &r.json_value,
        )
        .await;
        match result {
            Ok(()) => Ok(Response::new(PushRemediationResponse {
                success: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(PushRemediationResponse {
                success: false,
                error: e.to_string(),
            })),
        }
    }

    // ── CV7 T4-2: Sidecar registry ────────────────────────────────────────────
    // RegisterSidecar / SidecarHeartbeat. See src/sidecar_registry.rs and
    // docs/architecture/sidecars.md.

    async fn register_sidecar(
        &self,
        req: Request<RegisterSidecarRequest>,
    ) -> Result<Response<RegisterSidecarResponse>, Status> {
        self.require_capability(&req, SIDECAR_PROTOCOL)?;
        let r = req.into_inner();
        if r.name.trim().is_empty() {
            return Ok(Response::new(RegisterSidecarResponse {
                sidecar_id: String::new(),
                error: "name is required".into(),
            }));
        }
        if r.kind.trim().is_empty() {
            return Ok(Response::new(RegisterSidecarResponse {
                sidecar_id: String::new(),
                error: "kind is required".into(),
            }));
        }
        let sidecar_id = self
            .sidecar_registry
            .register(
                r.name.clone(),
                r.kind.clone(),
                r.version,
                r.capabilities,
                r.address,
            )
            .await;
        tracing::info!(
            sidecar_id = %sidecar_id,
            name = %r.name,
            kind = %r.kind,
            "sidecar registered"
        );
        Ok(Response::new(RegisterSidecarResponse {
            sidecar_id,
            error: String::new(),
        }))
    }

    async fn sidecar_heartbeat(
        &self,
        req: Request<SidecarHeartbeatRequest>,
    ) -> Result<Response<SidecarHeartbeatResponse>, Status> {
        self.require_capability(&req, SIDECAR_PROTOCOL)?;
        let r = req.into_inner();
        match self
            .sidecar_registry
            .heartbeat(
                &r.sidecar_id,
                r.events_in_total,
                r.detections_out_total,
                r.status_json,
            )
            .await
        {
            Ok(()) => Ok(Response::new(SidecarHeartbeatResponse {
                error: String::new(),
                reregister_required: false,
            })),
            // Unknown sidecar_id — bonsai was likely restarted. Ask the
            // sidecar to re-register rather than treating this as fatal.
            Err(()) => Ok(Response::new(SidecarHeartbeatResponse {
                error: String::new(),
                reregister_required: true,
            })),
        }
    }
}

pub fn managed_device_from_target(target: &TargetConfig) -> ManagedDevice {
    ManagedDevice {
        address: target.address.clone(),
        enabled: Some(target.enabled),
        tls_domain: target.tls_domain.clone().unwrap_or_default(),
        ca_cert: target.ca_cert.clone().unwrap_or_default(),
        vendor: target.vendor.clone().unwrap_or_default(),
        credential_alias: target.credential_alias.clone().unwrap_or_default(),
        username_env: target.username_env.clone().unwrap_or_default(),
        password_env: target.password_env.clone().unwrap_or_default(),
        hostname: target.hostname.clone().unwrap_or_default(),
        role: target.role.clone().unwrap_or_default(),
        site: target.site.clone().unwrap_or_default(),
        collector_id: target.collector_id.clone().unwrap_or_default(),
        selected_paths: target
            .selected_paths
            .iter()
            .cloned()
            .map(selected_path_to_proto)
            .collect(),
    }
}

pub fn target_from_managed_device(
    device: Option<ManagedDevice>,
) -> Result<TargetConfig, &'static str> {
    let device = device.ok_or("device is required")?;
    if device.address.trim().is_empty() {
        return Err("device.address is required");
    }

    Ok(TargetConfig {
        address: device.address,
        enabled: device.enabled.unwrap_or(true),
        tls_domain: option_string(device.tls_domain),
        ca_cert: option_string(device.ca_cert),
        vendor: option_string(device.vendor),
        credential_alias: option_string(device.credential_alias),
        username_env: option_string(device.username_env),
        password_env: option_string(device.password_env),
        username: None,
        password: None,
        hostname: option_string(device.hostname),
        role: option_string(device.role),
        site: option_string(device.site),
        collector_id: option_string(device.collector_id),
        selected_paths: device
            .selected_paths
            .into_iter()
            .map(selected_path_from_proto)
            .collect(),
        extra_ips: Vec::new(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
        external_id: None,
        inventory_only: false,
        paths: vec![],
        optional: false,
    })
}

fn selected_path_to_proto(path: SelectedSubscriptionPath) -> SubscriptionPath {
    SubscriptionPath {
        path: path.path,
        origin: path.origin,
        mode: path.mode,
        sample_interval_ns: path.sample_interval_ns,
        rationale: path.rationale,
        optional: path.optional,
    }
}

fn selected_path_from_proto(path: SubscriptionPath) -> SelectedSubscriptionPath {
    SelectedSubscriptionPath {
        path: path.path,
        origin: path.origin,
        mode: path.mode,
        sample_interval_ns: path.sample_interval_ns,
        rationale: path.rationale,
        optional: path.optional,
    }
}

fn site_to_proto(site: SiteRecord) -> Site {
    Site {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
    }
}

fn site_from_proto(site: Option<Site>) -> anyhow::Result<SiteRecord> {
    let site = site.ok_or_else(|| anyhow::anyhow!("site is required"))?;
    Ok(SiteRecord {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
        environment_id: String::new(),
    })
}

fn discovery_report_to_proto(report: discovery::DiscoveryReport) -> DiscoveryReport {
    DiscoveryReport {
        vendor_detected: report.vendor_detected,
        models_advertised: report.models_advertised,
        gnmi_encoding: report.gnmi_encoding,
        recommended_profiles: report
            .recommended_profiles
            .into_iter()
            .map(|profile| PathProfileMatch {
                profile_name: profile.profile_name,
                paths: profile
                    .paths
                    .into_iter()
                    .map(|path| SubscriptionPath {
                        path: path.path,
                        origin: path.origin,
                        mode: path.mode,
                        sample_interval_ns: path.sample_interval_ns,
                        rationale: path.rationale,
                        optional: path.optional,
                    })
                    .collect(),
                rationale: profile.rationale,
                confidence: profile.confidence,
            })
            .collect(),
        warnings: report.warnings,
    }
}

async fn target_conn_info_from_config(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> Result<TargetConnInfo, Status> {
    let ca_cert_pem = match &target.ca_cert {
        Some(path) => Some(
            crate::tls_util::read_cert_pem(path, credentials)
                .await
                .map_err(|e| Status::internal(format!("{e:#}")))?,
        ),
        None => None,
    };

    let resolved_credentials = resolve_target_credentials(target, credentials)
        .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => {
            let password = credentials.password_string();
            (Some(credentials.username), Some(password))
        }
        None => (None, None),
    };

    Ok(TargetConnInfo {
        address: target.address.clone(),
        username,
        password,
        ca_cert_pem,
        tls_domain: target.tls_domain.clone().unwrap_or_default(),
    })
}

fn credential_to_proto(credential: CredentialSummary) -> Credential {
    Credential {
        alias: credential.alias,
        created_at_ns: credential.created_at_ns,
        updated_at_ns: credential.updated_at_ns,
        last_used_at_ns: credential.last_used_at_ns,
    }
}

fn resolve_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials
            .resolve(alias, ResolvePurpose::Remediate)
            .map(Some);
    }

    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential {
                username,
                password: zeroize::Zeroizing::new(password),
            }),
            _ => None,
        },
    )
}

fn resolve_request_credentials(
    credentials: &CredentialVault,
    credential_alias: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = credential_alias {
        return credentials
            .resolve(&alias, ResolvePurpose::Discover)
            .map(Some);
    }

    let username = username_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    let password = password_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    Ok(match (username, password) {
        (Some(username), Some(password)) => Some(ResolvedCredential {
            username,
            password: zeroize::Zeroizing::new(password),
        }),
        _ => None,
    })
}

fn option_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// row-to-proto helpers

fn interface_from_row(row: &[Value]) -> Interface {
    Interface {
        device_address: str_val(&row[0]),
        name: str_val(&row[1]),
        in_pkts: i64_val(&row[2]),
        out_pkts: i64_val(&row[3]),
        in_octets: i64_val(&row[4]),
        out_octets: i64_val(&row[5]),
        in_errors: i64_val(&row[6]),
        out_errors: i64_val(&row[7]),
        updated_at_ns: ts_val(&row[8]),
    }
}

fn bgp_neighbor_from_row(row: &[Value]) -> BgpNeighbor {
    BgpNeighbor {
        device_address: str_val(&row[0]),
        peer_address: str_val(&row[1]),
        peer_as: i64_val(&row[2]),
        session_state: str_val(&row[3]),
        updated_at_ns: ts_val(&row[4]),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Int64(n) => serde_json::json!(*n),
        Value::TimestampNs(dt) => serde_json::json!(dt.unix_timestamp_nanos() as i64),
        _ => serde_json::Value::Null,
    }
}

fn str_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn i64_val(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

fn ts_val(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

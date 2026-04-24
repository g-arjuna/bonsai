use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use lbug::{Connection, Value};
use tokio_stream::wrappers::BroadcastStream;
use tonic::{Request, Response, Status, Streaming};

use crate::config::{SelectedSubscriptionPath, TargetConfig};
use crate::credentials::{CredentialSummary, CredentialVault, ResolvedCredential};
use crate::discovery;
use crate::event_bus::InProcessBus;
use crate::gnmi_set::gnmi_set;
use crate::graph::{GraphStore, SiteRecord};
use crate::ingest;
use crate::registry::{ApiRegistry, DeviceRegistry};
use crate::store::BonsaiStore;

pub const PROTOCOL_VERSION: u32 = 1;

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
    collector_manager: Option<Arc<crate::assignment::CollectorManager>>,
}

impl<S: BonsaiStore> BonsaiService<S> {
    pub fn new(
        store: Arc<S>,
        registry: Arc<ApiRegistry>,
        credentials: Arc<CredentialVault>,
        bus: Arc<InProcessBus>,
        collector_manager: Option<Arc<crate::assignment::CollectorManager>>,
    ) -> Self {
        Self {
            store,
            registry,
            credentials,
            bus,
            collector_manager,
        }
    }
}

pub type CollectorService = BonsaiService<crate::collector::graph::CollectorGraphStore>;
pub type CoreService = BonsaiService<GraphStore>;

type EventStream = Pin<Box<dyn Stream<Item = Result<pb::StateEvent, Status>> + Send>>;
type RegisterCollectorStream = Pin<Box<dyn Stream<Item = Result<pb::AssignmentUpdate, Status>> + Send>>;

#[tonic::async_trait]
impl<S: BonsaiStore + 'static> BonsaiGraph for BonsaiService<S> {
    type StreamEventsStream = EventStream;
    type RegisterCollectorStream = RegisterCollectorStream;

    async fn register_collector(
        &self,
        req: Request<CollectorIdentity>,
    ) -> Result<Response<Self::RegisterCollectorStream>, Status> {
        let identity = req.into_inner();
        let collector_id = identity.collector_id;
        let manager = self
            .collector_manager
            .as_ref()
            .ok_or_else(|| Status::unimplemented("collector manager not enabled on this node"))?;

        let mut rx = manager
            .register_collector(collector_id.clone())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let manager_for_stream = manager.clone();
        let collector_id_for_stream = collector_id.clone();

        let stream = async_stream::stream! {
            while let Some(update) = rx.recv().await {
                yield Ok(update);
            }
            // Cleanup on stream close
            manager_for_stream.unregister_collector(&collector_id_for_stream);
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn heartbeat(
        &self,
        req: Request<pb::CollectorStats>,
    ) -> Result<Response<pb::HeartbeatAck>, Status> {
        let stats = req.into_inner();
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
        _req: Request<GetDevicesRequest>,
    ) -> Result<Response<GetDevicesResponse>, Status> {
        let db = self.store.db();
        let devices = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let rows = conn
                .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname, d.updated_at")
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(Device {
                    address: str_val(&row[0]),
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
        _req: Request<pb::ListManagedDevicesRequest>,
    ) -> Result<Response<pb::ListManagedDevicesResponse>, Status> {
        let devices = self
            .registry
            .list_active()
            .map_err(|e| Status::internal(e.to_string()))?
            .into_iter()
            .map(|target| managed_device_from_target(&target))
            .collect();

        Ok(Response::new(pb::ListManagedDevicesResponse { devices }))
    }

    async fn add_device(
        &self,
        req: Request<pb::AddDeviceRequest>,
    ) -> Result<Response<pb::DeviceMutationResponse>, Status> {
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self.registry.add_device(target) {
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
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self.registry.update_device(target) {
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
        let request = req.into_inner();
        let credentials = resolve_request_credentials(
            &self.credentials,
            option_string(request.credential_alias),
            option_string(request.username_env),
            option_string(request.password_env),
        )
        .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
        let (username, password) = match credentials {
            Some(credentials) => (Some(credentials.username), Some(credentials.password)),
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
        };

        let report = discovery::discover_device(input)
            .await
            .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;

        Ok(Response::new(discovery_report_to_proto(report)))
    }

    async fn list_sites(
        &self,
        _req: Request<pb::ListSitesRequest>,
    ) -> Result<Response<pb::ListSitesResponse>, Status> {
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
        _req: Request<pb::ListCredentialsRequest>,
    ) -> Result<Response<pb::ListCredentialsResponse>, Status> {
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
        let device_address = req.into_inner().device_address;
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
        let device_address = req.into_inner().device_address;
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
        _req: Request<GetTopologyRequest>,
    ) -> Result<Response<GetTopologyResponse>, Status> {
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
        let inner = req.into_inner();
        let filter_types = inner.event_types;
        let filter_device = inner.device_address;
        let rx = self.store.subscribe_events();

        let stream = BroadcastStream::new(rx).filter_map(move |item| {
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
        });

        Ok(Response::new(Box::pin(stream)))
    }

    async fn telemetry_ingest(
        &self,
        req: Request<Streaming<pb::TelemetryIngestUpdate>>,
    ) -> Result<Response<pb::TelemetryIngestResponse>, Status> {
        let mut stream = req.into_inner();
        let mut accepted = 0_u64;

        while let Some(update) = stream
            .message()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            if accepted == 0 && update.protocol_version != PROTOCOL_VERSION {
                tracing::warn!(
                    collector_id = %update.collector_id,
                    client_version = update.protocol_version,
                    server_version = PROTOCOL_VERSION,
                    "protocol version skew detected"
                );
            }

            let collector_id = update.collector_id.clone();
            let telemetry = ingest::ingest_update_to_telemetry(update)
                .map_err(|e| Status::invalid_argument(format!("{e:#}")))?;
            self.bus.publish(telemetry);
            accepted += 1;

            if accepted == 1 || accepted.is_multiple_of(1_000) {
                tracing::debug!(%collector_id, accepted, "accepted telemetry ingest updates");
            }
        }

        Ok(Response::new(pb::TelemetryIngestResponse {
            accepted,
            error: String::new(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn detection_ingest(
        &self,
        req: Request<Streaming<pb::DetectionEventIngest>>,
    ) -> Result<Response<pb::DetectionIngestResponse>, Status> {
        let mut stream = req.into_inner();
        let mut accepted = 0_u64;

        while let Some(d) = stream
            .message()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            let collector_id = d.collector_id.clone();
            match self
                .store
                .write_detection(
                    d.device_address,
                    d.rule_id,
                    d.severity,
                    d.features_json,
                    d.fired_at_ns,
                    d.state_change_event_id,
                )
                .await
            {
                Ok(_) => {
                    accepted += 1;
                }
                Err(error) => {
                    tracing::warn!(%collector_id, %error, "failed to write ingested detection to graph");
                }
            }
        }

        Ok(Response::new(pb::DetectionIngestResponse {
            accepted,
            error: String::new(),
        }))
    }

    async fn create_detection(
        &self,
        req: Request<CreateDetectionRequest>,
    ) -> Result<Response<CreateDetectionResponse>, Status> {
        let r = req.into_inner();
        match self
            .store
            .write_detection(
                r.device_address,
                r.rule_id,
                r.severity,
                r.features_json,
                r.fired_at_ns,
                r.state_change_event_id,
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
    let ca_cert_pem =
        match &target.ca_cert {
            Some(path) => Some(tokio::fs::read(path).await.map_err(|e| {
                Status::internal(format!("could not read CA cert from '{path}': {e}"))
            })?),
            None => None,
        };

    let resolved_credentials = resolve_target_credentials(target, credentials)
        .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
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
        return credentials.resolve(alias).map(Some);
    }

    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
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
        return credentials.resolve(&alias).map(Some);
    }

    let username = username_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    let password = password_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    Ok(match (username, password) {
        (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
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


use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use lbug::{Connection, Value};
use tokio_stream::wrappers::BroadcastStream;
use tonic::{Request, Response, Status, Streaming};

use crate::config::TargetConfig;
use crate::discovery;
use crate::event_bus::InProcessBus;
use crate::gnmi_set::gnmi_set;
use crate::graph::{BonsaiEvent, GraphStore};
use crate::ingest;
use crate::registry::{ApiRegistry, DeviceRegistry};

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

pub struct BonsaiService {
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    bus: Arc<InProcessBus>,
}

impl BonsaiService {
    pub fn new(store: Arc<GraphStore>, registry: Arc<ApiRegistry>, bus: Arc<InProcessBus>) -> Self {
        Self {
            store,
            registry,
            bus,
        }
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<StateEvent, Status>> + Send>>;

#[tonic::async_trait]
impl BonsaiGraph for BonsaiService {
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
        _req: Request<ListManagedDevicesRequest>,
    ) -> Result<Response<ListManagedDevicesResponse>, Status> {
        let devices = self
            .registry
            .list_active()
            .map_err(|e| Status::internal(e.to_string()))?
            .into_iter()
            .map(|target| managed_device_from_target(&target))
            .collect();

        Ok(Response::new(ListManagedDevicesResponse { devices }))
    }

    async fn add_device(
        &self,
        req: Request<AddDeviceRequest>,
    ) -> Result<Response<DeviceMutationResponse>, Status> {
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self.registry.add_device(target) {
            Ok(target) => Ok(Response::new(DeviceMutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_from_target(&target)),
            })),
            Err(e) => Ok(Response::new(DeviceMutationResponse {
                success: false,
                error: e.to_string(),
                device: None,
            })),
        }
    }

    async fn update_device(
        &self,
        req: Request<UpdateDeviceRequest>,
    ) -> Result<Response<DeviceMutationResponse>, Status> {
        let target = target_from_managed_device(req.into_inner().device)
            .map_err(Status::invalid_argument)?;
        match self.registry.update_device(target) {
            Ok(target) => Ok(Response::new(DeviceMutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_from_target(&target)),
            })),
            Err(e) => Ok(Response::new(DeviceMutationResponse {
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
            Ok(Some(target)) => Ok(Response::new(DeviceMutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_from_target(&target)),
            })),
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
        req: Request<DiscoverRequest>,
    ) -> Result<Response<DiscoveryReport>, Status> {
        let request = req.into_inner();
        let input = discovery::DiscoveryInput {
            address: request.address,
            username_env: option_string(request.username_env),
            password_env: option_string(request.password_env),
            ca_cert_path: option_string(request.ca_cert_path),
            tls_domain: option_string(request.tls_domain),
            role_hint: option_string(request.role_hint),
        };

        let report = discovery::discover_device(input)
            .await
            .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;

        Ok(Response::new(discovery_report_to_proto(report)))
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

    type StreamEventsStream = EventStream;

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
                let ev: BonsaiEvent = item.ok()?;
                if !filter_device.is_empty() && ev.device_address != filter_device {
                    return None;
                }
                if !filter_types.is_empty() && !filter_types.contains(&ev.event_type) {
                    return None;
                }
                Some(Ok(StateEvent {
                    device_address: ev.device_address,
                    event_type: ev.event_type,
                    detail_json: ev.detail_json,
                    occurred_at_ns: ev.occurred_at_ns,
                    state_change_event_id: ev.state_change_event_id,
                }))
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    async fn telemetry_ingest(
        &self,
        req: Request<Streaming<TelemetryIngestUpdate>>,
    ) -> Result<Response<TelemetryIngestResponse>, Status> {
        let mut stream = req.into_inner();
        let mut accepted = 0_u64;

        while let Some(update) = stream
            .message()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            let collector_id = update.collector_id.clone();
            let telemetry = ingest::ingest_update_to_telemetry(update)
                .map_err(|e| Status::invalid_argument(format!("{e:#}")))?;
            self.bus.publish(telemetry);
            accepted += 1;

            if accepted == 1 || accepted.is_multiple_of(1_000) {
                tracing::debug!(%collector_id, accepted, "accepted telemetry ingest updates");
            }
        }

        Ok(Response::new(TelemetryIngestResponse {
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

        let conn_info = target_conn_info_from_config(&target).await?;
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

fn managed_device_from_target(target: &TargetConfig) -> ManagedDevice {
    ManagedDevice {
        address: target.address.clone(),
        tls_domain: target.tls_domain.clone().unwrap_or_default(),
        ca_cert: target.ca_cert.clone().unwrap_or_default(),
        vendor: target.vendor.clone().unwrap_or_default(),
        username_env: target.username_env.clone().unwrap_or_default(),
        password_env: target.password_env.clone().unwrap_or_default(),
        hostname: target.hostname.clone().unwrap_or_default(),
        role: target.role.clone().unwrap_or_default(),
        site: target.site.clone().unwrap_or_default(),
    }
}

fn target_from_managed_device(device: Option<ManagedDevice>) -> Result<TargetConfig, &'static str> {
    let device = device.ok_or("device is required")?;
    if device.address.trim().is_empty() {
        return Err("device.address is required");
    }

    Ok(TargetConfig {
        address: device.address,
        tls_domain: option_string(device.tls_domain),
        ca_cert: option_string(device.ca_cert),
        vendor: option_string(device.vendor),
        username_env: option_string(device.username_env),
        password_env: option_string(device.password_env),
        username: None,
        password: None,
        hostname: option_string(device.hostname),
        role: option_string(device.role),
        site: option_string(device.site),
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
                    })
                    .collect(),
                rationale: profile.rationale,
                confidence: profile.confidence,
            })
            .collect(),
        warnings: report.warnings,
    }
}

async fn target_conn_info_from_config(target: &TargetConfig) -> Result<TargetConnInfo, Status> {
    let ca_cert_pem =
        match &target.ca_cert {
            Some(path) => Some(tokio::fs::read(path).await.map_err(|e| {
                Status::internal(format!("could not read CA cert from '{path}': {e}"))
            })?),
            None => None,
        };

    Ok(TargetConnInfo {
        address: target.address.clone(),
        username: target.resolved_username(),
        password: target.resolved_password(),
        ca_cert_pem,
        tls_domain: target.tls_domain.clone().unwrap_or_default(),
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
